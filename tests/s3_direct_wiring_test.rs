//! 真实启动/签发链路门禁（RESUMABLE_UPLOAD_SPEC §2.2/§8.2/§8.7，第十六轮评审
//! P0）：不再靠手改 manifest 的夹具证明冻结字段，而是驱动**生产签发入口**
//! `issue_chunked_upload_token` + `install_s3_direct` 接线 + `FileHttpServer::new`
//! 装配，断言：
//!   1. 门禁开启且达阈值 → 真实签发先 `CreateMultipartUpload` 再写 manifest，
//!      全部冻结字段（含 `storage_source_id`）一次写成；
//!   2. 低于阈值 / 未接线 → 回退 proxy，绝不建 MPU；
//!   3. `CreateMultipartUpload` 失败 → 签发报错且不落本地会话目录；
//!   4. `FileHttpServer::new` 与扫描任务拿同一份接线（`Arc::ptr_eq`），不再恒 None；
//!   5. 用该接线跑一次 `sweep_expired_s3`，过期 S3 会话按生产编排被清理。
//!
//! 本文件不写 `privchat_file_uploads` 行（签发只占序列号与会话目录），
//! 无真库行卫生负担；仍需真库连接（`reserve_file_id` 用 PG 序列）。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sqlx::postgres::PgPoolOptions;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileHttpServer;
use privchat::rpc::error::RpcError;
use privchat::rpc::file::request_chunked_upload_token::{
    issue_chunked_upload_token, ChunkedTokenServices,
};
use privchat::service::chunked_upload::{self, s3_part_geometry, ChunkedSession, NewSession, S3SessionSetup};
use privchat::service::file_service::{FileService, S3DirectUploadWiring};
use privchat::service::final_object_probe::{FinalObjectHead, FinalObjectProbe, ProbeError};
use privchat::service::numbered_parts::{
    CompletedPart, ListedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};
use privchat::service::upload_token_service::UploadTokenService;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::FileRequestChunkedUploadTokenRequest;

const UPLOADER: u64 = 9_975_001;
const BUCKET: &str = "privchat-wiring";
const PREFIX: &str = "files";
const PROVIDER_UPLOAD_ID: &str = "mpu-issued-1";

// ---------- 记录 create 调用的 fake 后端（签发链路专用） ----------

#[derive(Clone, Debug)]
struct CreateCall {
    session_upload_id: String,
    bucket: String,
    final_key: String,
    total_size: u64,
}

struct IssuanceBackend {
    create_calls: Mutex<Vec<CreateCall>>,
    create_result: Mutex<Result<String, NumberedPartError>>,
}

impl IssuanceBackend {
    fn new() -> Self {
        Self {
            create_calls: Mutex::new(Vec::new()),
            create_result: Mutex::new(Ok(PROVIDER_UPLOAD_ID.to_string())),
        }
    }
    fn calls(&self) -> Vec<CreateCall> {
        self.create_calls.lock().unwrap().clone()
    }
    fn fail_create(&self) {
        *self.create_result.lock().unwrap() =
            Err(NumberedPartError::Backend("s3 down".into()));
    }
}

#[async_trait]
impl NumberedPartBackend for IssuanceBackend {
    async fn create(
        &self,
        session_upload_id: &str,
        bucket: &str,
        final_key: &str,
        total_size: u64,
    ) -> Result<UploadReference, NumberedPartError> {
        self.create_calls.lock().unwrap().push(CreateCall {
            session_upload_id: session_upload_id.to_string(),
            bucket: bucket.to_string(),
            final_key: final_key.to_string(),
            total_size,
        });
        let pid = self.create_result.lock().unwrap().clone()?;
        Ok(UploadReference {
            bucket: bucket.to_string(),
            final_key: final_key.to_string(),
            provider_upload_id: pid,
        })
    }
    async fn sign_part_url(
        &self,
        _reference: &UploadReference,
        _part_number: u32,
        _content_length: u64,
        _checksum_sha256_b64: &str,
        _ttl_secs: u64,
    ) -> Result<String, NumberedPartError> {
        Err(NumberedPartError::Backend("sign_part_url 不在本测试路径上".into()))
    }
    async fn list_parts(
        &self,
        _reference: &UploadReference,
    ) -> Result<Vec<ListedPart>, NumberedPartError> {
        // 扫描用例：abort 后 MPU 已彻底关闭。
        Err(NumberedPartError::NoSuchUpload)
    }
    async fn complete(
        &self,
        _reference: &UploadReference,
        _parts: &[CompletedPart],
    ) -> Result<(), NumberedPartError> {
        Err(NumberedPartError::Backend("complete 不在本测试路径上".into()))
    }
    async fn abort(&self, _reference: &UploadReference) -> Result<(), NumberedPartError> {
        Ok(())
    }
}

/// 最小对象探测：签发/装配用例不碰它；扫描用例里 HEAD 空 = 无残留对象。
struct IdleProbe;

#[async_trait]
impl FinalObjectProbe for IdleProbe {
    async fn head(
        &self,
        _reference: &UploadReference,
    ) -> Result<Option<FinalObjectHead>, ProbeError> {
        Ok(None)
    }
    async fn sha256_of(&self, _reference: &UploadReference) -> Result<String, ProbeError> {
        Err(ProbeError::Backend("sha256_of 不在本测试路径上".into()))
    }
    async fn delete_if_match(
        &self,
        _reference: &UploadReference,
        _etag: &str,
    ) -> Result<bool, ProbeError> {
        Err(ProbeError::Backend("delete_if_match 不在本测试路径上".into()))
    }
}

// ---------- rig ----------

struct Rig {
    file_service: Arc<FileService>,
    upload_token_service: UploadTokenService,
    backend: Arc<IssuanceBackend>,
    _dir: tempfile::TempDir,
}

async fn make_rig(wired: bool) -> Rig {
    let url = privchat::require_test_database_url()
        .expect("真库门禁需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL");
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(2)
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
    let file_service = Arc::new(FileService::new(vec![source], 0, pool));
    file_service.init().await.expect("init storage");
    let backend = Arc::new(IssuanceBackend::new());
    if wired {
        file_service.install_s3_direct(S3DirectUploadWiring {
            source_id: 1,
            bucket: BUCKET.to_string(),
            path_prefix: PREFIX.to_string(),
            backend: backend.clone(),
            probe: Arc::new(IdleProbe),
        });
    }
    Rig {
        file_service,
        upload_token_service: UploadTokenService::new(),
        backend,
        _dir: dir,
    }
}

fn services(rig: &Rig) -> ChunkedTokenServices<'_> {
    ChunkedTokenServices {
        file_service: &rig.file_service,
        upload_token_service: &rig.upload_token_service,
        file_api_base_url: Some("http://e2e.local/files"),
        s3_direct_threshold: 16 << 20,
    }
}

fn req(file_size: i64) -> FileRequestChunkedUploadTokenRequest {
    FileRequestChunkedUploadTokenRequest {
        file_type: "file".to_string(),
        business_type: "message".to_string(),
        file_size,
        file_hash: format!("ab{file_size:062x}"),
        mime_type: "application/octet-stream".to_string(),
        filename: Some("payload.bin".to_string()),
        transform_version: 0,
        force_upload: true,
        supported_upload_transports: Some(vec![
            "proxy_offset_v1".to_string(),
            "s3_multipart_v1".to_string(),
        ]),
    }
}

// ================= 1. 真实签发链路 =================

/// 🔴 生产签发入口（非手改 manifest 的夹具）：门禁开 + 达阈值 → 先
/// `CreateMultipartUpload`（记录会话 id）再写 manifest，全部冻结字段一次写成。
#[tokio::test]
async fn real_issuance_creates_mpu_then_freezes_all_fields() {
    let rig = make_rig(true).await;
    let size: i64 = 32 << 20;
    let resp = issue_chunked_upload_token(&services(&rig), UPLOADER, req(size))
        .await
        .expect("真实签发应成功");
    assert_eq!(resp.transport.as_deref(), Some("s3_multipart_v1"));
    let (part_size, total_parts) = s3_part_geometry(size as u64);
    assert_eq!(resp.part_size, Some(part_size), "响应片大小 = 冻结公式");
    assert_eq!(resp.total_parts, Some(total_parts), "响应片数 = 冻结公式");

    // CreateMultipartUpload 发生在写 manifest 之前，且参数与会话一致。
    let token = resp.upload_token.expect("token");
    let upload_id = token.split('.').next().expect("upload id");
    let calls = rig.backend.calls();
    assert_eq!(calls.len(), 1, "签发必须且只能建一次 MPU");
    let call = &calls[0];
    assert_eq!(call.session_upload_id, upload_id, "MPU metadata 记会话 id");
    assert_eq!(call.bucket, BUCKET);
    assert_eq!(call.total_size, size as u64);
    assert!(
        call.final_key.starts_with(&format!("{PREFIX}/")),
        "final_key 走接线 path_prefix 拼接"
    );

    // manifest 冻结字段全部来自本次签发（含 storage_source_id，§3.2）。
    let root = rig.file_service.upload_session_root().expect("session root");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("chunked").join(upload_id).join("manifest.json"))
            .expect("read manifest"),
    )
    .expect("parse manifest");
    assert_eq!(manifest["transport"], "s3_multipart_v1");
    assert_eq!(manifest["storage_source_id"], serde_json::json!(1));
    assert_eq!(manifest["bucket"], serde_json::json!(BUCKET));
    assert_eq!(manifest["final_key"], serde_json::json!(call.final_key));
    assert_eq!(manifest["provider_upload_id"], serde_json::json!(PROVIDER_UPLOAD_ID));
    assert_eq!(manifest["part_size"], serde_json::json!(part_size));
    assert_eq!(manifest["total_parts"], serde_json::json!(total_parts));
}

/// 低于阈值 → 回退 proxy：不建 MPU，manifest 无任何 S3 冻结字段。
#[tokio::test]
async fn below_threshold_falls_back_to_proxy_without_creating_mpu() {
    let rig = make_rig(true).await;
    let resp = issue_chunked_upload_token(&services(&rig), UPLOADER, req(1 << 20))
        .await
        .expect("小文件签发应成功");
    assert_eq!(resp.transport.as_deref(), Some("proxy_offset_v1"));
    assert!(resp.part_size.is_none() && resp.total_parts.is_none());
    assert!(rig.backend.calls().is_empty(), "回退路径绝不建 MPU");

    let token = resp.upload_token.expect("token");
    let upload_id = token.split('.').next().expect("upload id");
    let root = rig.file_service.upload_session_root().expect("session root");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("chunked").join(upload_id).join("manifest.json"))
            .expect("read manifest"),
    )
    .expect("parse manifest");
    assert!(manifest["storage_source_id"].is_null(), "proxy 会话无冻结存储源");
    assert!(manifest["provider_upload_id"].is_null());
}

/// `CreateMultipartUpload` 失败 → 签发报错，不落本地会话目录（无半建目录泄漏）。
#[tokio::test]
async fn issuance_fails_without_leaving_a_session_when_create_mpu_fails() {
    let rig = make_rig(true).await;
    rig.backend.fail_create();
    let err = issue_chunked_upload_token(&services(&rig), UPLOADER, req(32 << 20))
        .await
        .expect_err("MPU 建不了必须报错");
    assert!(
        matches!(err, RpcError { code: ErrorCode::InternalError, .. }),
        "MPU 创建失败应映射为内部错误"
    );
    assert!(err.message.contains("S3"));
    assert_eq!(rig.backend.calls().len(), 1);
    // chunked/ 下不该多出任何会话目录。
    let root = rig.file_service.upload_session_root().expect("session root");
    let chunked = root.join("chunked");
    let count = std::fs::read_dir(&chunked)
        .map(|rd| rd.count())
        .unwrap_or(0);
    assert_eq!(count, 0, "签发失败不得留下半建会话目录");
}

// ================= 2. 启动装配：HTTP 服务器与扫描共用一份接线 =================

/// 🔴 `FileHttpServer::new` 从 `s3_direct()` 拿真实接线（第十六轮评审 P0：
/// 不再恒 None），且与签发/扫描共用同一份 `Arc`（`ptr_eq`）。
#[tokio::test]
async fn file_http_server_wires_backend_and_probe_from_s3_direct() {
    let rig = make_rig(true).await;
    let server = FileHttpServer::new(
        rig.file_service.clone(),
        Arc::new(UploadTokenService::new()),
        None,
        0,
    );
    let state = server.state();
    let wiring = rig.file_service.s3_direct().expect("接线必须存在");
    let backend = state.numbered_part_backend.as_ref().expect("后端已接线");
    let probe = state.final_object_probe.as_ref().expect("探测已接线");
    assert!(Arc::ptr_eq(&wiring.backend, backend), "HTTP 与接线同一份后端");
    assert!(Arc::ptr_eq(&wiring.probe, probe), "HTTP 与接线同一份探测");
    assert_eq!(wiring.source_id, 1);
    assert_eq!(wiring.bucket, BUCKET);

    // 未接线的进程：装配结果保持 None（端点回「门禁未接入」）。
    let unwired = make_rig(false).await;
    let server = FileHttpServer::new(
        unwired.file_service.clone(),
        Arc::new(UploadTokenService::new()),
        None,
        0,
    );
    assert!(server.state().numbered_part_backend.is_none());
    assert!(server.state().final_object_probe.is_none());
}

/// 扫描任务按 `server.rs` 的同一口径（`file_service.s3_direct()`）拿接线：
/// 过期且无墓碑的 S3 会话经生产编排被清掉，证明扫描不再是 `None, None` 空转。
#[tokio::test]
async fn sweep_with_installed_wiring_cleans_expired_s3_session() {
    let rig = make_rig(true).await;
    let wiring = rig.file_service.s3_direct().expect("接线必须存在");
    let root = rig.file_service.upload_session_root().expect("session root");

    // 用真实建会话原语建一个带全部冻结字段的过期 S3 会话。
    let (session, token, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: UPLOADER,
            total_size: 10 << 20,
            sealed_sha256: "0".repeat(64),
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "payload.bin".into(),
            mime_type: "application/octet-stream".into(),
            transform_version: 0,
            reserved_file_id: 9_975_901,
            transport: "s3_multipart_v1".to_string(),
            s3: Some(S3SessionSetup {
                part_size: 8 << 20,
                total_parts: 2,
                bucket: BUCKET.to_string(),
                final_key: format!("{PREFIX}/files/sweep.bin"),
                provider_upload_id: PROVIDER_UPLOAD_ID.to_string(),
                storage_source_id: 1,
            }),
        },
    )
    .expect("建 S3 会话");
    drop(session);
    let dir = root.join("chunked").join(token.split('.').next().expect("upload id"));
    // 改成过期。
    let manifest_path = dir.join("manifest.json");
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read")).expect("parse");
    m.as_object_mut()
        .expect("object")
        .insert("expires_at".into(), serde_json::json!(1));
    std::fs::write(&manifest_path, m.to_string()).expect("rewrite");

    // 与 server.rs 扫描循环同一份接线。
    let removed = chunked_upload::sweep_expired_s3(
        &root,
        Some(&wiring.backend),
        Some(&wiring.probe),
        &rig.file_service,
    )
    .await;
    assert_eq!(removed, 1, "接线后扫描必须真的清掉过期 S3 会话");
    assert!(!dir.exists());
}
