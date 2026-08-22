//! part-url 有效 S3 分支的门禁（RESUMABLE_UPLOAD_SPEC §8.3，实现顺序第 2 步）。
//!
//! 模式冲突的三种拒绝在 `part_url_mode_conflict_test.rs`；这里用 fake backend
//! 覆盖**放行之后**的行为：合法批量签发、checksum Base64 原样进 required_headers、
//! 几何/摘要错误不触达 backend、`NoSuchUpload` 映射 20613、完成后不再签发。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::service::chunked_upload::{ChunkedSession, NewSession};
use privchat::service::file_service::FileService;
use privchat::service::numbered_parts::{
    CompletedPart, ListedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};
use privchat::service::upload_token_service::UploadTokenService;

const UPLOADER: u64 = 9_973_002;
const PART_SIZE: u64 = 4 << 20;
const TOTAL_SIZE: u64 = 10 << 20; // 3 片：4 + 4 + 2 MiB
const TOTAL_PARTS: u32 = 3;

#[derive(Debug, Clone)]
struct SignCall {
    reference: UploadReference,
    part_number: u32,
    content_length: u64,
    checksum_sha256_b64: String,
    ttl_secs: u64,
}

/// 只实现 sign_part_url 的假后端：记录每次调用，可被拨成 NoSuchUpload。
struct FakeBackend {
    calls: Mutex<Vec<SignCall>>,
    fail_with: Mutex<Option<NumberedPartError>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self { calls: Mutex::new(Vec::new()), fail_with: Mutex::new(None) }
    }
    fn fail_with(&self, e: NumberedPartError) {
        *self.fail_with.lock().unwrap() = Some(e);
    }
    fn calls(&self) -> Vec<SignCall> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl NumberedPartBackend for FakeBackend {
    async fn create(
        &self,
        _session_upload_id: &str,
        _bucket: &str,
        _final_key: &str,
        _total_size: u64,
    ) -> Result<UploadReference, NumberedPartError> {
        Err(NumberedPartError::Backend("create 不在本测试路径上".into()))
    }
    async fn sign_part_url(
        &self,
        reference: &UploadReference,
        part_number: u32,
        content_length: u64,
        checksum_sha256_b64: &str,
        ttl_secs: u64,
    ) -> Result<String, NumberedPartError> {
        if let Some(e) = self.fail_with.lock().unwrap().clone() {
            return Err(e);
        }
        self.calls.lock().unwrap().push(SignCall {
            reference: reference.clone(),
            part_number,
            content_length,
            checksum_sha256_b64: checksum_sha256_b64.to_string(),
            ttl_secs,
        });
        Ok(format!("https://s3.fake/{part_number}?sig=xx"))
    }
    async fn list_parts(
        &self,
        _reference: &UploadReference,
    ) -> Result<Vec<ListedPart>, NumberedPartError> {
        Err(NumberedPartError::Backend("list_parts 不在本测试路径上".into()))
    }
    async fn complete(
        &self,
        _reference: &UploadReference,
        _parts: &[CompletedPart],
    ) -> Result<(), NumberedPartError> {
        Err(NumberedPartError::Backend("complete 不在本测试路径上".into()))
    }
    async fn abort(&self, _reference: &UploadReference) -> Result<(), NumberedPartError> {
        Err(NumberedPartError::Backend("abort 不在本测试路径上".into()))
    }
}

struct Rig {
    state: FileServerState,
    fake: Arc<FakeBackend>,
    _dir: tempfile::TempDir,
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
        direct_upload: None,
        region: None,
    };
    let file_service = FileService::new(vec![source], 0, pool);
    file_service.init().await.expect("init storage");
    let fake = Arc::new(FakeBackend::new());
    Rig {
        state: FileServerState {
            file_service: Arc::new(file_service),
            upload_token_service: Arc::new(UploadTokenService::new()),
            auth: None,
            numbered_part_backend: Some(fake.clone()),
            final_object_probe: None,
        },
        fake,
        _dir: dir,
    }
}

/// 建一个会话并把 manifest 改写成第 5 步建 S3 会话后的形态：
/// transport=s3_multipart_v1 + 分片参数 + spec 冻结的 bucket/final_key/provider_upload_id
/// 三个平铺字段。
async fn create_s3_session(rig: &Rig) -> (ChunkedSession, String) {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let (session, token, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: UPLOADER,
            total_size: TOTAL_SIZE,
            sealed_sha256: "cd".repeat(32),
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "payload.bin".into(),
            mime_type: "application/octet-stream".into(),
            transform_version: 0,
            reserved_file_id: 9_973_901,
            transport: "s3_multipart_v1".to_string(),
            s3: None,
        },
    )
    .expect("create session");
    // create() 对 S3 参数一律写 None（第 5 步才填）：这里手工补成建好 MPU 的样子。
    let upload_id = token.split('.').next().expect("upload id");
    let manifest_path = root.join("chunked").join(upload_id).join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    let obj = manifest.as_object_mut().expect("object");
    obj.insert("part_size".into(), serde_json::json!(PART_SIZE));
    obj.insert("total_parts".into(), serde_json::json!(TOTAL_PARTS));
    obj.insert("bucket".into(), serde_json::json!("privchat-e2e"));
    obj.insert("final_key".into(), serde_json::json!("files/s3-payload.bin"));
    obj.insert("provider_upload_id".into(), serde_json::json!("mpu-abc-123"));
    std::fs::write(&manifest_path, manifest.to_string()).expect("rewrite manifest");
    (session, token)
}

async fn post_part_url(rig: &Rig, token: &str, body: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/api/app/files/part-url")
        .header("X-Upload-Token", token)
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("build request");
    let resp = privchat::http::routes::upload::create_route()
        .with_state(rig.state.clone())
        .oneshot(req)
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json envelope");
    (status, json)
}

fn b64_of_hex(hex_str: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(hex::decode(hex_str).expect("hex"))
}

/// 合法批量：逐片签发、URL 来自 backend、checksum 的 Base64 **原样**进
/// required_headers（客户端 PUT 时照抄），TTL 冻结 15 分钟，引用三件套透传。
#[tokio::test]
async fn valid_batch_signs_urls_and_returns_checksum_headers_verbatim() {
    let rig = rig().await;
    let (_, token) = create_s3_session(&rig).await;
    let hex1 = "01".repeat(32);
    let hex3 = "ab".repeat(32);
    let body = serde_json::json!({
        "parts": [
            { "part_number": 1, "content_length": PART_SIZE, "checksum_sha256_hex": hex1 },
            { "part_number": 3, "content_length": 2u64 << 20, "checksum_sha256_hex": hex3 }
        ]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::OK, "合法批量必须放行：{json}");
    let parts = &json["data"]["parts"];
    assert_eq!(parts.as_array().expect("parts 数组").len(), 2);
    assert_eq!(parts[0]["url"], "https://s3.fake/1?sig=xx");
    assert_eq!(parts[1]["url"], "https://s3.fake/3?sig=xx");
    // 🔴 checksum 编码冻结：hex → RFC 4648 标准 Base64（保留 padding）原样返回。
    assert_eq!(parts[0]["required_headers"]["x-amz-checksum-sha256"], b64_of_hex(&hex1));
    assert_eq!(parts[1]["required_headers"]["x-amz-checksum-sha256"], b64_of_hex(&hex3));

    let calls = rig.fake.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].part_number, 1);
    assert_eq!(calls[0].content_length, PART_SIZE);
    assert_eq!(calls[1].content_length, 2 << 20);
    assert_eq!(calls[0].ttl_secs, 15 * 60, "TTL 冻结 15 分钟");
    // manifest 里的三件套必须原样透传给 backend（不得靠进程内映射）。
    assert_eq!(calls[0].reference.bucket, "privchat-e2e");
    assert_eq!(calls[0].reference.final_key, "files/s3-payload.bin");
    assert_eq!(calls[0].reference.provider_upload_id, "mpu-abc-123");
}

/// 几何 / 摘要 / 批量校验都发生在 backend 之前：参数非法一次都不触达 backend。
#[tokio::test]
async fn geometry_checksum_and_batch_errors_never_reach_the_backend() {
    let rig = rig().await;
    let (_, token) = create_s3_session(&rig).await;

    // 长度不对（非末片必须 = part_size）：20617，backend 零调用。
    let body = serde_json::json!({
        "parts": [{ "part_number": 1, "content_length": PART_SIZE - 1, "checksum_sha256_hex": "0".repeat(64) }]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["code"], 20617, "几何不对是未对齐，不是参数错");

    // checksum 不是 64 位 hex：400，backend 零调用。
    let body = serde_json::json!({
        "parts": [{ "part_number": 1, "content_length": PART_SIZE, "checksum_sha256_hex": "0".repeat(63) }]
    });
    let (status, _) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // 空批量：400，backend 零调用。
    let (status, _) = post_part_url(&rig, &token, r#"{"parts":[]}"#).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    assert!(rig.fake.calls().is_empty(), "参数校验失败绝不能触达 backend");
}

/// MPU 已关闭（NoSuchUpload）→ 20613 会话作废、从零重来；不是归属证明，不删除。
#[tokio::test]
async fn backend_no_such_upload_maps_to_session_gone() {
    let rig = rig().await;
    let (_, token) = create_s3_session(&rig).await;
    rig.fake.fail_with(NumberedPartError::NoSuchUpload);
    let body = serde_json::json!({
        "parts": [{ "part_number": 1, "content_length": PART_SIZE, "checksum_sha256_hex": "0".repeat(64) }]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(json["code"], 20613);
}

/// 完成墓碑之后不再签发：迟到的预签名对结果没有意义（20614，backend 零调用）。
#[tokio::test]
async fn completed_session_no_longer_signs() {
    let rig = rig().await;
    let (session, token) = create_s3_session(&rig).await;
    session.write_completed(9_973_901).expect("写完成墓碑");
    let body = serde_json::json!({
        "parts": [{ "part_number": 1, "content_length": PART_SIZE, "checksum_sha256_hex": "0".repeat(64) }]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], 20614);
    assert!(rig.fake.calls().is_empty(), "已完成的会话不该再触达 backend");
}
