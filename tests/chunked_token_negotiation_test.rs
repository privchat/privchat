//! 协商门禁（RESUMABLE_UPLOAD_SPEC §8.2，第十轮评审）：直接驱动**生产接线**
//! `issue_chunked_upload_token`（RPC 处理器剥出来的窄依赖版本，不是复制品）。
//!
//! 五个用例：
//!   1. 不带 supported_upload_transports → 响应保持旧格式（无 transport 等新字段）
//!   2. `[proxy_offset_v1]` → transport = proxy_offset_v1
//!   3. `[proxy_offset_v1, s3_multipart_v1]` 且门禁关闭 → 回退 proxy_offset_v1
//!   4. `[]` → 参数错误
//!   5. `[s3_multipart_v1]` → 参数错误
//! 外加顺序用例：非法集合 + 秒传命中 → 仍然参数错误（🔴 校验在秒传预检之前，
//! 同一非法请求不因文件是否已存在而改变结局）。

use std::sync::Arc;

use bytes::Bytes;
use futures::FutureExt as _;
use sha2::Digest as _;
use sqlx::postgres::PgPoolOptions;

use privchat::config::FileStorageSourceConfig;
use privchat::rpc::error::RpcError;
use privchat::rpc::file::request_chunked_upload_token::{
    issue_chunked_upload_token, ChunkedTokenServices,
};
use privchat::service::file_service::FileService;
use privchat::service::upload_token_service::UploadTokenService;
use privchat::service::FileType;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::FileRequestChunkedUploadTokenRequest;

const UPLOADER: u64 = 9_972_001;

struct Rig {
    pool: Arc<sqlx::PgPool>,
    file_service: FileService,
    upload_token_service: UploadTokenService,
    _dir: tempfile::TempDir,
}

/// 🔴 真库卫生：本测试的秒传对照用例会往 `privchat_file_uploads` 写正式行，而
/// TempDir 销毁后物理对象就没了——不清库就会留下「有记录、无对象」的垃圾行，
/// 后续重跑的秒传对照可能靠陈旧记录假绿，也会污染共享真库的其他测试。
/// 按 uploader 全量清：测试开头清一次（吃掉上次 panic 留下的残留），结尾再清一次。
async fn cleanup(pool: &sqlx::PgPool) {
    let _ = sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = $1")
        .bind(UPLOADER as i64)
        .execute(pool)
        .await;
}

async fn rig() -> Rig {
    let url = privchat::require_test_database_url()
        .expect("真库门禁需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL");
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let source = FileStorageSourceConfig {
        id: 0,
        storage_type: "local".to_string(),
        storage_root: dir.path().to_string_lossy().to_string(),
        base_url: Some("http://e2e.local/files".to_string()),
        endpoint: None,
        bucket: None,
        access_key_id: None,
        secret_access_key: None,
        path_prefix: None,
    };
    let file_service = FileService::new(vec![source], 0, pool.clone());
    file_service.init().await.expect("init storage");
    cleanup(&pool).await;
    Rig {
        pool,
        file_service,
        upload_token_service: UploadTokenService::new(),
        _dir: dir,
    }
}

fn services(rig: &Rig) -> ChunkedTokenServices<'_> {
    ChunkedTokenServices {
        file_service: &rig.file_service,
        upload_token_service: &rig.upload_token_service,
        file_api_base_url: Some("http://e2e.local/files"),
    }
}

fn req(file_hash: &str, transports: Option<Vec<&str>>) -> FileRequestChunkedUploadTokenRequest {
    FileRequestChunkedUploadTokenRequest {
        file_type: "file".to_string(),
        business_type: "message".to_string(),
        file_size: 1 << 20,
        file_hash: file_hash.to_string(),
        mime_type: "application/octet-stream".to_string(),
        filename: None,
        transform_version: 0,
        force_upload: false,
        supported_upload_transports: transports
            .map(|list| list.into_iter().map(str::to_string).collect()),
    }
}

/// 每个用例一个不会撞到库里已有内容的摘要。
fn fresh_hash(tag: &str) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"chunked-token-negotiation ");
    hasher.update(tag.as_bytes());
    hex::encode(sha2::Digest::finalize(hasher))
}

#[tokio::test]
async fn legacy_request_keeps_the_old_response_shape() {
    let rig = rig().await;
    let resp = issue_chunked_upload_token(&services(&rig), UPLOADER, req(&fresh_hash("legacy"), None))
        .await
        .expect("旧客户端请求应成功");
    assert!(!resp.already_exists);
    assert!(resp.upload_token.is_some());
    assert!(resp.upload_url.is_some());
    assert!(resp.transport.is_none(), "旧客户端响应不得带 transport");
    assert!(resp.part_size.is_none());
    assert!(resp.total_parts.is_none());
    // 序列化层面再锁一遍：wire JSON 里不允许出现任何新键。
    let v = serde_json::to_value(&resp).expect("serialize");
    for key in ["transport", "part_size", "total_parts"] {
        assert!(v.get(key).is_none(), "旧响应 wire JSON 不得含 {}", key);
    }
}

#[tokio::test]
async fn declaring_only_proxy_returns_proxy() {
    let rig = rig().await;
    let resp = issue_chunked_upload_token(
        &services(&rig),
        UPLOADER,
        req(&fresh_hash("only-proxy"), Some(vec!["proxy_offset_v1"])),
    )
    .await
    .expect("声明 proxy 应成功");
    assert_eq!(resp.transport.as_deref(), Some("proxy_offset_v1"));
    assert!(resp.part_size.is_none() && resp.total_parts.is_none());
}

#[tokio::test]
async fn declaring_both_falls_back_to_proxy_while_gates_are_closed() {
    let rig = rig().await;
    let resp = issue_chunked_upload_token(
        &services(&rig),
        UPLOADER,
        req(
            &fresh_hash("both"),
            Some(vec!["proxy_offset_v1", "s3_multipart_v1"]),
        ),
    )
    .await
    .expect("声明两种 transport 应成功");
    // direct_upload 配置 + 阈值 + 集成门禁均未接入（实现顺序第 5 步），恒回退 proxy。
    assert_eq!(resp.transport.as_deref(), Some("proxy_offset_v1"));
}

#[tokio::test]
async fn empty_transport_set_is_rejected() {
    let rig = rig().await;
    let err = issue_chunked_upload_token(
        &services(&rig),
        UPLOADER,
        req(&fresh_hash("empty"), Some(vec![])),
    )
    .await
    .expect_err("空集合必须被拒绝");
    assert!(matches!(err, RpcError { code: ErrorCode::InvalidParams, .. }));
}

#[tokio::test]
async fn s3_only_transport_set_is_rejected() {
    let rig = rig().await;
    let err = issue_chunked_upload_token(
        &services(&rig),
        UPLOADER,
        req(&fresh_hash("s3-only"), Some(vec!["s3_multipart_v1"])),
    )
    .await
    .expect_err("只声明 S3 必须被拒绝");
    assert!(matches!(err, RpcError { code: ErrorCode::InvalidParams, .. }));
}

/// 🔴 顺序门禁：「字段存在必须含 proxy_offset_v1」是**无条件协议约束**——先把一份
/// 文件真的传上去让秒传可命中，然后用同一个摘要带非法集合再请求：仍然必须参数错误，
/// 而不是因秒传命中而放行。反向验证：同一摘要的旧格式请求正常命中秒传。
#[tokio::test]
async fn invalid_transport_set_is_rejected_even_when_dedup_would_hit() {
    let rig = rig().await;
    // 失败路径也必须清库：用 catch_unwind 接住用例主体（含断言 panic），先清掉
    // 本 uploader 的行，再把失败原样重新抛出；双保险：下次 rig() 开头还会再清一次。
    let outcome = std::panic::AssertUnwindSafe(ordering_case_core(&rig))
        .catch_unwind()
        .await;
    cleanup(&rig.pool).await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

async fn ordering_case_core(rig: &Rig) {
    let bytes: &[u8] = b"privchat negotiation ordering fixture";
    let sha = hex::encode(sha2::Sha256::digest(bytes));

    // 走真实流式上传把文件落库，保证 find_by_content 能命中。
    let mut upload = rig
        .file_service
        .begin_streaming_upload(
            "application/octet-stream",
            "ordering.bin",
            bytes.len() as i64,
            None,
            Some(FileType::from_str("file").expect("file type")),
            UPLOADER,
            "ordering-session",
        )
        .await
        .expect("begin streaming upload");
    upload
        .write_chunk(Bytes::copy_from_slice(bytes))
        .await
        .expect("write chunk");
    rig.file_service
        .commit_streaming_upload(
            upload,
            "ordering.bin".to_string(),
            "application/octet-stream".to_string(),
            UPLOADER,
            None,
            "message".to_string(),
            None,
            0,
            None,
            0,
            Some(sha.clone()),
            Some(bytes.len() as i64),
        )
        .await
        .expect("commit streaming upload");

    // 非法集合 → 即便秒传会命中，也必须参数错误（校验在秒传预检之前）。
    let err = issue_chunked_upload_token(
        &services(rig),
        UPLOADER,
        req(&sha, Some(vec!["s3_multipart_v1"])),
    )
    .await
    .expect_err("秒传命中也不能放行非法集合");
    assert!(matches!(err, RpcError { code: ErrorCode::InvalidParams, .. }));

    // 反向对照：同一摘要的旧格式请求确实命中秒传，证明文件存在、上面的拒绝
    // 不是因为「文件不存在」。
    let resp = issue_chunked_upload_token(&services(rig), UPLOADER, req(&sha, None))
        .await
        .expect("旧格式请求应成功");
    assert!(resp.already_exists, "同摘要旧请求应命中秒传");
    assert!(resp.claim_token.is_some());
}
