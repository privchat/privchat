//! 真实 S3 兼容服务集成门禁（RESUMABLE_UPLOAD_SPEC §8.7，第十七轮评审 P1）：
//! `direct_upload` 是启动级开关，放行前必须在真实服务（本地 MinIO 或目标 S3）
//! 上验证生产后端的全部冻结行为，不能只靠 Fake：
//!   1. `CreateMultipartUpload` 写入归属 metadata 与 `ChecksumAlgorithm=SHA256`；
//!   2. 预签名 `UploadPart` 把 `x-amz-checksum-sha256` 签进 URL（错误值必须被拒）；
//!   3. `ListParts` 分页正确、每片回读 checksum（断点续传的依据）；
//!   4. `CompleteMultipartUpload` 的 `If-None-Match: *`（final key no-clobber）；
//!   5. ETag 条件删除（归属核对 + 防 TOCTOU）；
//!   6. `NoSuchUpload` 的 provider 真实错误映射与幂等 abort。
//!
//! 门禁：需要环境变量 `PRIVCHAT_S3_LIVE_ENDPOINT` / `_ACCESS_KEY` / `_SECRET_KEY`
//! / `_BUCKET`（可选 `_REGION`，默认 us-east-1）；缺失则整套跳过（离线单测照跑）。
//! 本地 MinIO 参考：
//! ```sh
//! minio server /tmp/minio-data --address 127.0.0.1:19000
//! export PRIVCHAT_S3_LIVE_ENDPOINT=http://127.0.0.1:19000
//! export PRIVCHAT_S3_LIVE_ACCESS_KEY=privchat-test
//! export PRIVCHAT_S3_LIVE_SECRET_KEY=privchat-test-secret-1
//! export PRIVCHAT_S3_LIVE_BUCKET=privchat-live-test
//! cargo test --test s3_backend_live_test
//! ```

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use sha2::Digest as _;

use privchat::config::FileStorageSourceConfig;
use privchat::service::file_service::FileService;
use privchat::service::final_object_probe::FinalObjectProbe;
use privchat::service::numbered_parts::{
    CompletedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};
use privchat::service::s3_backend::S3DirectBackend;
use sqlx::postgres::PgPoolOptions;

struct LiveEnv {
    endpoint: String,
    access_key: String,
    secret_key: String,
    bucket: String,
    region: String,
}

fn live_env() -> Option<LiveEnv> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (Some(endpoint), Some(access_key), Some(secret_key), Some(bucket)) = (
        get("PRIVCHAT_S3_LIVE_ENDPOINT"),
        get("PRIVCHAT_S3_LIVE_ACCESS_KEY"),
        get("PRIVCHAT_S3_LIVE_SECRET_KEY"),
        get("PRIVCHAT_S3_LIVE_BUCKET"),
    ) else {
        eprintln!("跳过：未配置 PRIVCHAT_S3_LIVE_* 环境变量（真实 S3 集成门禁未启用）");
        return None;
    };
    Some(LiveEnv {
        endpoint,
        access_key,
        secret_key,
        bucket,
        region: get("PRIVCHAT_S3_LIVE_REGION").unwrap_or_else(|| "us-east-1".to_string()),
    })
}

/// 按冻结入口 `from_source` 构建（fail-fast 配置校验同生产）。
fn make_backend(env: &LiveEnv, page_size: u32) -> Arc<S3DirectBackend> {
    let src = FileStorageSourceConfig {
        id: 99,
        storage_type: "s3".to_string(),
        storage_root: String::new(),
        base_url: None,
        endpoint: Some(env.endpoint.clone()),
        bucket: Some(env.bucket.clone()),
        access_key_id: Some(env.access_key.clone()),
        secret_access_key: Some(env.secret_key.clone()),
        path_prefix: None,
        direct_upload: Some("s3_multipart_v1".to_string()),
        region: Some(env.region.clone()),
    };
    Arc::new(
        S3DirectBackend::from_source(&src)
            .expect("live 配置完整，构建不应失败")
            .with_list_page_size(page_size),
    )
}

/// 桶不存在则创建（测试自持，不依赖外部预置）。
async fn ensure_bucket(env: &LiveEnv) {
    let signer = reqsign::AwsV4Signer::new("s3", &env.region);
    let cred = reqsign::AwsCredential {
        access_key_id: env.access_key.clone(),
        secret_access_key: env.secret_key.clone(),
        session_token: None,
        expires_in: None,
    };
    let url = format!("{}/{}", env.endpoint.trim_end_matches('/'), env.bucket);
    let mut req = reqwest::Client::new()
        .put(&url)
        .build()
        .expect("build PutBucket");
    signer.sign(&mut req, &cred).expect("sign PutBucket");
    let resp = reqwest::Client::new()
        .execute(req)
        .await
        .expect("PutBucket 请求失败");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success() || body.contains("BucketAlreadyOwnedByYou") || body.contains("BucketAlreadyExists"),
        "建桶失败: HTTP {} body={body}",
        status.as_u16()
    );
}

fn unique_key(label: &str) -> String {
    format!("live-test/{label}-{}", hex::encode(rand::random::<[u8; 8]>()))
}

fn sha256_b64(data: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(data))
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

/// 按预签名 URL 上传一片（客户端口径：必须原样携带签发时的 checksum 头）。
async fn put_part(url: &str, data: &[u8], checksum_b64: &str) -> reqwest::Response {
    reqwest::Client::new()
        .put(url)
        .header("x-amz-checksum-sha256", checksum_b64)
        .body(data.to_vec())
        .send()
        .await
        .expect("PUT 分片请求失败")
}

/// 传一片并断言成功，返回 complete 需要的三字段。
async fn upload_part(
    backend: &S3DirectBackend,
    reference: &UploadReference,
    part_number: u32,
    data: &[u8],
) -> CompletedPart {
    let checksum = sha256_b64(data);
    let url = backend
        .sign_part_url(reference, part_number, data.len() as u64, &checksum, 600)
        .await
        .expect("预签名 UploadPart");
    let resp = put_part(&url, data, &checksum).await;
    let status = resp.status();
    let etag = resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .trim_matches('"')
        .to_string();
    let body = resp.text().await.unwrap_or_default();
    assert!(status.is_success(), "上传分片 {part_number} 失败: HTTP {} body={body}", status.as_u16());
    assert!(!etag.is_empty(), "UploadPart 响应必须带 ETag");
    CompletedPart {
        part_number,
        etag,
        checksum_sha256_b64: checksum,
    }
}

const PART_SIZE: usize = 6 << 20; // 6 MiB：满足 S3「非末片 ≥ 5 MiB」

fn part_data(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add((i % 251) as u8)).collect()
}

// ================= 1. 完整上传 + 断点 + 分页 + checksum 回读 =================

/// 断点续传口径：先传 1、3 片，`ListParts` 只回这两片（页大小 2 → 真实分页）；
/// 补传第 2 片后 complete；最终对象带归属 metadata、逐片 checksum 全程可验。
#[tokio::test]
async fn live_multipart_resume_pagination_checksum_and_metadata() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 2);
    let key = unique_key("resume");
    let session_id = format!("live-{}", hex::encode(rand::random::<[u8; 8]>()));

    let p1 = part_data(1, PART_SIZE);
    let p2 = part_data(2, PART_SIZE);
    let p3 = part_data(3, 100 << 10); // 末片可小于 5 MiB
    let full: Vec<u8> = p1.iter().chain(&p2).chain(&p3).copied().collect();

    let reference = backend
        .create(&session_id, &env.bucket, &key, full.len() as u64)
        .await
        .expect("真实 CreateMultipartUpload");
    assert!(!reference.provider_upload_id.is_empty());

    // 断点：先传 1、3。
    let c1 = upload_part(&backend, &reference, 1, &p1).await;
    let c3 = upload_part(&backend, &reference, 3, &p3).await;

    // ListParts 回读 = 断点续传依据：只有 1、3，且逐片 checksum 与大小正确。
    let listed = backend.list_parts(&reference).await.expect("ListParts");
    let nums: Vec<u32> = listed.iter().map(|p| p.part_number).collect();
    assert_eq!(nums, vec![1, 3], "断点恢复只看已上传分片");
    assert_eq!(listed[0].size, PART_SIZE as u64);
    assert_eq!(listed[1].size, (100 << 10) as u64);
    assert_eq!(
        listed[0].checksum_sha256_b64.as_deref(),
        Some(sha256_b64(&p1).as_str()),
        "CreateMPU 声明 SHA256 后，ListParts 必须回读片 checksum"
    );
    assert_eq!(
        listed[1].checksum_sha256_b64.as_deref(),
        Some(sha256_b64(&p3).as_str())
    );

    // 补传第 2 片，三片齐全后 complete。
    let c2 = upload_part(&backend, &reference, 2, &p2).await;
    backend
        .complete(&reference, &[c1.clone(), c2, c3])
        .await
        .expect("CompleteMultipartUpload");

    // HEAD：归属 metadata + 长度；内容身份 = 整文件摘要（§3.5 回读口径）。
    let head = backend
        .head(&reference)
        .await
        .expect("HEAD final")
        .expect("complete 后对象必须存在");
    assert_eq!(head.content_length, full.len() as u64);
    assert_eq!(
        head.privchat_upload_id.as_deref(),
        Some(session_id.as_str()),
        "CreateMPU 写入的归属 metadata 必须随 final 对象保留"
    );
    assert!(!head.etag.is_empty());
    assert_eq!(
        backend.sha256_of(&reference).await.expect("回读"),
        sha256_hex(&full),
        "回读摘要 = 整文件摘要（文件身份唯一权威）"
    );

    // ETag 条件删除（第十八轮评审 P1：不得把「对象被删」当「条件生效」）：
    // - 支持条件删除的后端：硬断言 错 ETag → Ok(false) 且对象必须在；对 ETag → Ok(true)。
    // - 不支持的后端（如 MinIO）：本用例不降级验收条件删除（直接清理收尾），
    //   能力缺失由启动期诊断告警暴露（第二十一轮起不再拒启动），运营监控决定放行。
    let supported = backend
        .probe_conditional_delete(&env.bucket)
        .await
        .expect("能力探测");
    if supported {
        assert_eq!(
            backend
                .delete_if_match(&reference, "\"etag-does-not-match\"")
                .await
                .expect("条件删除(错条件)"),
            false,
            "ETag 不符必须拒绝删除"
        );
        assert!(
            backend.head(&reference).await.unwrap().is_some(),
            "错 ETag 删除后对象必须仍在（条件真正生效的唯一证据）"
        );
        assert_eq!(
            backend
                .delete_if_match(&reference, &head.etag)
                .await
                .expect("条件删除(对条件)"),
            true
        );
    } else {
        // 能力缺失不在这里兜底：直接清掉测试对象，门禁结论见门禁用例。
        let _ = backend.delete_if_match(&reference, &head.etag).await;
    }
    assert!(backend.head(&reference).await.unwrap().is_none(), "结束后对象消失");
}

// ================= 1b. 启动诊断：缺安全能力的 provider 照常接线 + 告警（不拒启动） =================

/// 🔴 第二十一轮评审（运营策略）：启动期能力探测降级为诊断告警——缺任一项能力，
/// `FileService::init` 照常接线放行（服务正常启动），上传期失败返回错误码并写日志，
/// 不回退内置上传（单一数据面）。运行时安全语义不变：删除仍走 If-Match、
/// complete 仍带 If-None-Match。本地 MinIO 实测命中告警分支（条件删除不支持）。
#[tokio::test]
async fn live_gate_wires_provider_with_missing_capabilities_and_warns() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 1000);
    let cond_delete = backend
        .probe_conditional_delete(&env.bucket)
        .await
        .expect("条件删除能力探测");
    let no_clobber = backend
        .probe_complete_no_clobber(&env.bucket)
        .await
        .expect("no-clobber 能力探测");
    println!("live provider 能力：conditional_delete={cond_delete} complete_no_clobber={no_clobber}");

    let src = FileStorageSourceConfig {
        id: 99,
        storage_type: "s3".to_string(),
        storage_root: String::new(),
        base_url: None,
        endpoint: Some(env.endpoint.clone()),
        bucket: Some(env.bucket.clone()),
        access_key_id: Some(env.access_key.clone()),
        secret_access_key: Some(env.secret_key.clone()),
        path_prefix: None,
        direct_upload: Some("s3_multipart_v1".to_string()),
        region: Some(env.region.clone()),
    };
    let url = privchat::require_test_database_url()
        .expect("门禁用例需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL");
    let pool = Arc::new(PgPoolOptions::new().max_connections(1).connect(&url).await.expect("连库"));
    let service = FileService::new(vec![src], 99, pool);
    // 🔴 无论能力齐备与否，启动都必须成功且接线生效（能力缺失只告警，不拒启动）。
    service.init().await.expect("能力探测不再阻塞启动");
    assert!(service.s3_direct().is_some(), "接线必须生效（单一数据面，上传期失败再报错）");
    // 🔴 第二十二轮：诊断异步化后，init 应在接线完成后立即返回；
    // 给它一个宽裕窗口核对（真实后端上两项探测本身很快）。
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(service.s3_direct().is_some(), "异步诊断不得影响已落位的接线");
}

/// 🔴 第二十二轮评审：后端网络不可达时启动不得被阻塞。用不可路由地址：
/// 若诊断同步跑，探测会挂到客户端/总超时；异步化后 `init` 必须立即返回且接线生效，
/// 诊断在后台超时后只写告警日志（离线可跑，不需要 live 环境）。
#[tokio::test]
async fn init_returns_promptly_when_s3_endpoint_unreachable() {
    let src = FileStorageSourceConfig {
        id: 98,
        storage_type: "s3".to_string(),
        storage_root: String::new(),
        base_url: None,
        endpoint: Some("http://10.255.255.1:19999".to_string()),
        bucket: Some("privchat-unreachable".to_string()),
        access_key_id: Some("privchat-test".to_string()),
        secret_access_key: Some("privchat-test-secret-1".to_string()),
        path_prefix: None,
        direct_upload: Some("s3_multipart_v1".to_string()),
        region: Some("us-east-1".to_string()),
    };
    let url = privchat::require_test_database_url()
        .expect("该用例需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL");
    let pool = Arc::new(PgPoolOptions::new().max_connections(1).connect(&url).await.expect("连库"));
    let service = FileService::new(vec![src], 98, pool);
    let started = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(5), service.init())
        .await
        .expect("init 不得被不可达后端阻塞（诊断必须异步 + 带超时）")
        .expect("配置合法，init 本身应成功（诊断失败只告警）");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "init 必须立即返回，实际耗时 {:?}",
        started.elapsed()
    );
    assert!(service.s3_direct().is_some(), "后端不可达不影响接线（上传期失败再报错，单一数据面）");
}

// ================= 2. 预签名 checksum：错误值必须被拒 =================

/// 预签名把 `x-amz-checksum-sha256` 签进 URL：客户端改成别的值上传，
/// 无论以签名不符(403)还是 checksum 不符(400)的形式，都必须被拒。
#[tokio::test]
async fn live_part_put_with_wrong_checksum_is_rejected() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 1000);
    let key = unique_key("badsum");
    let reference = backend
        .create("live-badsum", &env.bucket, &key, PART_SIZE as u64)
        .await
        .expect("CreateMPU");

    let data = part_data(7, PART_SIZE);
    let right = sha256_b64(&data);
    let url = backend
        .sign_part_url(&reference, 1, data.len() as u64, &right, 600)
        .await
        .expect("预签名");
    let wrong = sha256_b64(b"something else");
    let resp = put_part(&url, &data, &wrong).await;
    assert!(
        !resp.status().is_success(),
        "checksum 与预签名值不一致的上传必须被拒，实际 HTTP {}",
        resp.status().as_u16()
    );

    // 拒收的分片不得计入：ListParts 为空。
    let listed = backend.list_parts(&reference).await.expect("ListParts");
    assert!(listed.is_empty(), "被拒分片不得落库");
    backend.abort(&reference).await.expect("清理 MPU");
}

// ================= 3. If-None-Match：final key no-clobber =================

/// final key 已有对象时，第二次 complete 必须被 `If-None-Match: *` 拒绝
/// （409/412 映射为 Conflict/PreconditionFailed），且原对象毫发无损。
#[tokio::test]
async fn live_complete_if_none_match_rejects_overwrite() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 1000);
    let key = unique_key("noclobber");
    let data = part_data(11, PART_SIZE);

    // 先正常传完一次。
    let first = backend
        .create("live-first", &env.bucket, &key, data.len() as u64)
        .await
        .expect("第一次 CreateMPU");
    let c = upload_part(&backend, &first, 1, &data).await;
    backend.complete(&first, &[c]).await.expect("第一次 complete");
    let etag_before = backend.head(&first).await.unwrap().expect("对象在").etag;

    // 同一 final key 再建 MPU 传一片，complete 必须被拒。
    let second = backend
        .create("live-second", &env.bucket, &key, data.len() as u64)
        .await
        .expect("第二次 CreateMPU");
    let c2 = upload_part(&backend, &second, 1, &part_data(12, PART_SIZE)).await;
    let err = backend
        .complete(&second, &[c2])
        .await
        .expect_err("If-None-Match 必须拒绝覆盖已有对象");
    assert!(
        matches!(
            err,
            NumberedPartError::Conflict | NumberedPartError::PreconditionFailed
        ),
        "no-clobber 拒绝应映射为 Conflict/PreconditionFailed，实际 {err:?}"
    );

    // 原对象未被覆盖：内容摘要不变。
    assert_eq!(backend.sha256_of(&first).await.unwrap(), sha256_hex(&data));
    assert_eq!(backend.head(&first).await.unwrap().unwrap().etag, etag_before);

    // 🔴 第十九轮评审 P0：启动探测必须同样能证明这个能力（门禁完整性）：
    // 本 provider 既然真实拒绝了覆盖，探测也必须判支持。
    assert!(
        backend
            .probe_complete_no_clobber(&env.bucket)
            .await
            .expect("no-clobber 能力探测"),
        "真实拒绝了覆盖的 provider，探测必须判支持"
    );

    // 清理：第二次 MPU 作废 + 删对象。
    backend.abort(&second).await.expect("abort 第二次 MPU");
    assert_eq!(backend.delete_if_match(&first, &etag_before).await.unwrap(), true);
}

// ================= 4. NoSuchUpload 真实映射与幂等 abort =================

/// provider 实际错误格式：对不存在的 uploadId，`ListParts`/`Complete` 必须映射为
/// `NoSuchUpload`（扫描器确认循环与恢复路径都依赖这个映射）；`abort` 幂等。
#[tokio::test]
async fn live_no_such_upload_mapping_and_idempotent_abort() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 1000);
    let reference = UploadReference {
        bucket: env.bucket.clone(),
        final_key: unique_key("ghost"),
        provider_upload_id: "ghost-upload-id-000".to_string(),
    };
    assert!(
        matches!(
            backend.list_parts(&reference).await,
            Err(NumberedPartError::NoSuchUpload)
        ),
        "ListParts 对不存在的 MPU 必须映射 NoSuchUpload"
    );
    // 🔴 带非空分片列表：部分实现（如 MinIO）对空列表先报参数错，
    // 带上分片才会走到查 uploadId 的 NoSuchUpload 路径（扫描器恢复路径的真实形态）。
    let ghost_part = vec![CompletedPart {
        part_number: 1,
        etag: "ghost-etag".to_string(),
        checksum_sha256_b64: sha256_b64(b"ghost"),
    }];
    assert!(
        matches!(
            backend.complete(&reference, &ghost_part).await,
            Err(NumberedPartError::NoSuchUpload)
        ),
        "Complete 对不存在的 MPU 必须映射 NoSuchUpload"
    );
    backend
        .abort(&reference)
        .await
        .expect("abort 不存在的 MPU 幂等 = Ok");
}

// ================= 5. abort 后确认：扫描器口径 =================

/// 扫描器判据 20 的真实版：传一片 → abort → `ListParts` 确认（`NoSuchUpload`）→
/// HEAD final 为空，全程无任何残留。
#[tokio::test]
async fn live_abort_then_confirm_nothing_left() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let backend = make_backend(&env, 1000);
    let key = unique_key("abort");
    let reference = backend
        .create("live-abort", &env.bucket, &key, PART_SIZE as u64)
        .await
        .expect("CreateMPU");
    upload_part(&backend, &reference, 1, &part_data(21, PART_SIZE)).await;
    assert_eq!(backend.list_parts(&reference).await.unwrap().len(), 1);

    backend.abort(&reference).await.expect("abort");
    assert!(
        matches!(
            backend.list_parts(&reference).await,
            Err(NumberedPartError::NoSuchUpload)
        ),
        "abort 后 ListParts 确认 = NoSuchUpload"
    );
    assert!(
        backend.head(&reference).await.expect("HEAD").is_none(),
        "abort 不得产生 final 对象"
    );
}
