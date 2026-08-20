//! S3 分流门禁（RESUMABLE_UPLOAD_SPEC §8.3/§8.5，实现顺序第 3 步）。
//!
//! 用 fake 分片后端 + fake 对象探测覆盖 status/complete/abort 的 S3 分支：
//! complete 全程（HEAD 三分支 → 缺片预检 → 409/412 恢复 → 回读 → 建行）、
//! status 的 ListParts 换算与 NoSuchUpload HEAD 恢复、abort 的确认后才删目录，
//! 以及 S3 会话串用 /files/chunk 的 20616 终局。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::service::chunked_upload::{ChunkedSession, NewSession};
use privchat::service::file_service::FileService;
use privchat::service::final_object_probe::{FinalObjectHead, FinalObjectProbe, ProbeError};
use privchat::service::numbered_parts::{
    CompletedPart, ListedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};
use privchat::service::upload_token_service::UploadTokenService;

const PART_SIZE: u64 = 4 << 20;
const TOTAL_SIZE: u64 = 10 << 20; // 3 片：4 + 4 + 2 MiB
const TOTAL_PARTS: u32 = 3;
// 每个用例独立摘要：真库共享，同摘要会在 converge_upload 里互相判重。
fn sealed_of(file_id: u64) -> String {
    format!("aa{file_id:062x}")
}

fn size_of(n: u32) -> u64 {
    if n == TOTAL_PARTS {
        TOTAL_SIZE - PART_SIZE * (TOTAL_PARTS as u64 - 1)
    } else {
        PART_SIZE
    }
}

fn part(n: u32) -> ListedPart {
    ListedPart {
        part_number: n,
        size: size_of(n),
        etag: format!("etag-{n}"),
        checksum_sha256_b64: Some(format!("checksum-{n}")),
    }
}

fn all_parts() -> Vec<ListedPart> {
    (1..=TOTAL_PARTS).map(part).collect()
}

// ---------- fake 分片后端 ----------

struct FakeBackend {
    list_queue: Mutex<VecDeque<Result<Vec<ListedPart>, NumberedPartError>>>,
    list_default: Mutex<Result<Vec<ListedPart>, NumberedPartError>>,
    complete_result: Mutex<Result<(), NumberedPartError>>,
    abort_result: Mutex<Result<(), NumberedPartError>>,
    abort_calls: AtomicU32,
    list_calls: AtomicU32,
    complete_calls: Mutex<Vec<Vec<CompletedPart>>>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            list_queue: Mutex::new(VecDeque::new()),
            list_default: Mutex::new(Ok(all_parts())),
            complete_result: Mutex::new(Ok(())),
            abort_result: Mutex::new(Ok(())),
            abort_calls: AtomicU32::new(0),
            list_calls: AtomicU32::new(0),
            complete_calls: Mutex::new(Vec::new()),
        }
    }
    fn set_list(&self, parts: Vec<ListedPart>) {
        *self.list_default.lock().unwrap() = Ok(parts);
    }
    fn set_list_err(&self, e: NumberedPartError) {
        *self.list_default.lock().unwrap() = Err(e);
    }
    fn queue_list(&self, r: Result<Vec<ListedPart>, NumberedPartError>) {
        self.list_queue.lock().unwrap().push_back(r);
    }
    fn set_complete_result(&self, e: NumberedPartError) {
        *self.complete_result.lock().unwrap() = Err(e);
    }
    fn set_abort_result(&self, e: NumberedPartError) {
        *self.abort_result.lock().unwrap() = Err(e);
    }
    fn abort_calls(&self) -> u32 {
        self.abort_calls.load(Ordering::SeqCst)
    }
    fn complete_call_parts(&self) -> Vec<Vec<CompletedPart>> {
        self.complete_calls.lock().unwrap().clone()
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
        self.list_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(next) = self.list_queue.lock().unwrap().pop_front() {
            return next;
        }
        self.list_default.lock().unwrap().clone()
    }
    async fn complete(
        &self,
        _reference: &UploadReference,
        parts: &[CompletedPart],
    ) -> Result<(), NumberedPartError> {
        let r = self.complete_result.lock().unwrap().clone();
        if r.is_ok() {
            self.complete_calls.lock().unwrap().push(parts.to_vec());
        }
        r
    }
    async fn abort(&self, _reference: &UploadReference) -> Result<(), NumberedPartError> {
        self.abort_calls.fetch_add(1, Ordering::SeqCst);
        self.abort_result.lock().unwrap().clone()
    }
}

// ---------- fake 对象探测 ----------

struct FakeProbe {
    head_queue: Mutex<VecDeque<Result<Option<FinalObjectHead>, ProbeError>>>,
    head: Mutex<Result<Option<FinalObjectHead>, ProbeError>>,
    sha256: Mutex<Result<String, ProbeError>>,
    delete_result: Mutex<Result<(), ProbeError>>,
    delete_calls: AtomicU32,
}

impl FakeProbe {
    fn new() -> Self {
        Self {
            head_queue: Mutex::new(VecDeque::new()),
            head: Mutex::new(Ok(None)),
            sha256: Mutex::new(Ok(String::new())),
            delete_result: Mutex::new(Ok(())),
            delete_calls: AtomicU32::new(0),
        }
    }
    /// 按调用次序下发的 HEAD 结果（用完回落到 `set_head` 的默认值）：
    /// 412/秒传用例需要「第一次 HEAD 空、后续命中」的时序。
    fn queue_head(&self, upload_id: Option<&str>, content_length: u64) {
        self.head_queue.lock().unwrap().push_back(Ok(Some(FinalObjectHead {
            content_length,
            privchat_upload_id: upload_id.map(|s| s.to_string()),
        })));
    }
    /// 队列里压一个「不存在」：complete 的第 3 步 HEAD 必须先消耗掉它，
    /// 后续恢复点的 HEAD 才能命中。
    fn queue_head_none(&self) {
        self.head_queue.lock().unwrap().push_back(Ok(None));
    }
    fn set_head(&self, upload_id: Option<&str>, content_length: u64) {
        *self.head.lock().unwrap() = Ok(Some(FinalObjectHead {
            content_length,
            privchat_upload_id: upload_id.map(|s| s.to_string()),
        }));
    }
    fn set_sha256(&self, sha: &str) {
        *self.sha256.lock().unwrap() = Ok(sha.to_string());
    }
    fn fail_delete(&self) {
        *self.delete_result.lock().unwrap() = Err(ProbeError::Backend("delete 失败".into()));
    }
    fn delete_calls(&self) -> u32 {
        self.delete_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl FinalObjectProbe for FakeProbe {
    async fn head(
        &self,
        _reference: &UploadReference,
    ) -> Result<Option<FinalObjectHead>, ProbeError> {
        if let Some(next) = self.head_queue.lock().unwrap().pop_front() {
            return next;
        }
        self.head.lock().unwrap().clone()
    }
    async fn sha256_of(&self, _reference: &UploadReference) -> Result<String, ProbeError> {
        self.sha256.lock().unwrap().clone()
    }
    async fn delete(&self, _reference: &UploadReference) -> Result<(), ProbeError> {
        self.delete_calls.fetch_add(1, Ordering::SeqCst);
        self.delete_result.lock().unwrap().clone()
    }
}

// ---------- rig ----------

struct Rig {
    state: FileServerState,
    backend: Arc<FakeBackend>,
    probe: Arc<FakeProbe>,
    pool: Arc<PgPool>,
    _dir: tempfile::TempDir,
}

async fn make_rig() -> Rig {
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
    let backend = Arc::new(FakeBackend::new());
    let probe = Arc::new(FakeProbe::new());
    Rig {
        state: FileServerState {
            file_service: Arc::new(file_service),
            upload_token_service: Arc::new(UploadTokenService::new()),
            auth: None,
            numbered_part_backend: Some(backend.clone()),
            final_object_probe: Some(probe.clone()),
        },
        backend,
        probe,
        pool,
        _dir: dir,
    }
}

/// 真库卫生（第十一轮评审冻结的口径）：清理返回 Result，残留 ≠ 0 即失败。
async fn cleanup(pool: &PgPool, uploader: u64) -> Result<(), String> {
    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = $1")
        .bind(uploader as i64)
        .execute(pool)
        .await
        .map_err(|e| format!("cleanup DELETE 失败: {e}"))?;
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1")
            .bind(uploader as i64)
            .fetch_one(pool)
            .await
            .map_err(|e| format!("cleanup 复查失败: {e}"))?;
    if remaining != 0 {
        return Err(format!("cleanup 后仍残留 {remaining} 行 uploader_id={uploader}"));
    }
    Ok(())
}

/// 建 S3 会话并把 manifest 改写成建好 MPU 的形态（平铺三字段，§8.7）。
async fn create_s3_session(rig: &Rig, uploader: u64, reserved_file_id: u64) -> (ChunkedSession, String) {
    create_s3_session_with_sha(rig, uploader, reserved_file_id, sealed_of(reserved_file_id)).await
}

async fn create_s3_session_with_sha(
    rig: &Rig,
    uploader: u64,
    reserved_file_id: u64,
    sealed: String,
) -> (ChunkedSession, String) {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let (session, token, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: uploader,
            total_size: TOTAL_SIZE,
            sealed_sha256: sealed,
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "payload.bin".into(),
            mime_type: "application/octet-stream".into(),
            transform_version: 0,
            reserved_file_id,
            transport: "s3_multipart_v1".to_string(),
        },
    )
    .expect("create session");
    let upload_id = token.split('.').next().expect("upload id").to_string();
    let manifest_path = root.join("chunked").join(&upload_id).join("manifest.json");
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

fn upload_id_of(token: &str) -> &str {
    token.split('.').next().expect("upload id")
}

async fn call(rig: &Rig, req: Request<Body>) -> (StatusCode, serde_json::Value) {
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

fn get_status(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/app/files/status")
        .header("X-Upload-Token", token)
        .body(Body::empty())
        .expect("build request")
}

fn post_complete(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/app/files/complete")
        .header("X-Upload-Token", token)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"encryption_version":0}"#))
        .expect("build request")
}

fn post_abort(token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/api/app/files/abort")
        .header("X-Upload-Token", token)
        .body(Body::empty())
        .expect("build request")
}

// ================= status =================

/// ListParts → 现有区间格式：缺片只报缺失区间，协议零改动（§8.3）。
#[tokio::test]
async fn s3_status_converts_parts_to_ranges() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_900).await;
    // 只有第 1、3 片。
    rig.backend.set_list(vec![part(1), part(3)]);
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let data = &json["data"];
    assert_eq!(data["completed"], false);
    assert_eq!(data["received_bytes"], PART_SIZE + size_of(3));
    assert_eq!(data["received"][0]["offset"], 0);
    assert_eq!(data["received"][0]["length"], PART_SIZE);
    assert_eq!(data["received"][1]["offset"], 2 * PART_SIZE);
    assert_eq!(data["received"][1]["length"], size_of(3));
    assert_eq!(data["missing"][0]["offset"], PART_SIZE);
    assert_eq!(data["missing"][0]["length"], PART_SIZE);
}

/// 🔴 长度异常或缺摘要的片一律视为缺失（不进 received，§8.3），不得产出
/// 语义不明的区间——否则 status 报完整而 complete 永远过不去。
#[tokio::test]
async fn s3_status_treats_bad_or_checksumless_parts_as_missing() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_901).await;
    let mut bad_size = part(2);
    bad_size.size = PART_SIZE - 1; // 非末片长度异常
    let mut no_checksum = part(3);
    no_checksum.checksum_sha256_b64 = None;
    rig.backend.set_list(vec![part(1), bad_size, no_checksum]);
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let data = &json["data"];
    assert_eq!(data["received"].as_array().unwrap().len(), 1, "只有第 1 片合法：{json}");
    assert_eq!(data["received_bytes"], PART_SIZE);
    assert_eq!(data["missing"][0]["offset"], PART_SIZE);
    assert_eq!(data["missing"][0]["length"], TOTAL_SIZE - PART_SIZE);
}

/// NoSuchUpload + HEAD 命中本 session 且长度一致 → 报完整，由客户端照常
/// complete（判据 26）；长度不符不得报完整；metadata 不符回 500 保留对象。
#[tokio::test]
async fn s3_status_no_such_upload_head_recovery() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_902).await;
    let upload_id = upload_id_of(&token).to_string();
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);

    // HEAD 命中 + 归属 + 长度一致 → received=[0,total)、missing=[]、completed=false。
    rig.probe.set_head(Some(&upload_id), TOTAL_SIZE);
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let data = &json["data"];
    assert_eq!(data["completed"], false, "status 不建行不写墓碑，completed 必须 false");
    assert_eq!(data["missing"].as_array().unwrap().len(), 0);
    assert_eq!(data["received"][0]["offset"], 0);
    assert_eq!(data["received"][0]["length"], TOTAL_SIZE);

    // 长度不符 → 可重试 5xx，绝不把不完整对象报成完整。
    rig.probe.set_head(Some(&upload_id), TOTAL_SIZE - 1);
    let (status, _) = call(&rig, get_status(&token)).await;
    assert!(status.is_server_error(), "长度不符必须回可重试 5xx");

    // metadata 不属于本 session → 500 保留对象。
    rig.probe.set_head(Some("another-session"), TOTAL_SIZE);
    let (status, _) = call(&rig, get_status(&token)).await;
    assert!(status.is_server_error());

    // HEAD 不存在 → 20613 会话作废。
    *rig.probe.head.lock().unwrap() = Ok(None);
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::GONE);
    assert_eq!(json["code"], 20613);
}

// ================= complete =================

/// 全链路：HEAD 空 → 快照齐全 → complete Ok → 回读一致 → 建行 + 墓碑。
/// 重复 complete 走墓碑幂等回同一 file_id。
#[tokio::test]
async fn s3_complete_happy_path_creates_row_and_tombstone() {
    const UPLOADER: u64 = 9_974_101;
    const FILE_ID: u64 = 9_974_101_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
    // 回读摘要 = 会话声明的 sealed 才算身份一致（每个用例独立摘要）。
    rig.probe.set_sha256(&sealed_of(FILE_ID));

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "全链路必须成功：{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);

    // PG 行已建：hash = 回读摘要、file_path = manifest 的 final_key。
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT file_path, file_hash FROM privchat_file_uploads WHERE file_id = $1")
            .bind(FILE_ID as i64)
            .fetch_optional(&*rig.pool)
            .await
            .expect("query row");
    let (path, hash) = row.expect("PG 行必须存在");
    assert_eq!(path, "files/s3-payload.bin");
    assert_eq!(hash.as_deref(), Some(sealed_of(FILE_ID).as_str()));

    // Complete 提交的三字段快照来自 ListParts（S3 自己的记录）。
    let calls = rig.backend.complete_call_parts();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), TOTAL_PARTS as usize);
    assert_eq!(calls[0][0].etag, "etag-1");
    assert_eq!(calls[0][0].checksum_sha256_b64, "checksum-1");

    // 墓碑幂等：重复 complete 回同一 file_id，不再触达 backend。
    let before = rig.backend.complete_call_parts().len();
    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.complete_call_parts().len(), before, "墓碑后不得再调 backend");

    cleanup(&rig.pool, UPLOADER)
        .await
        .expect("用例后清库必须成功，否则真库污染会在绿灯下持续累积");
}

/// 缺片 complete → 409 回缺失区间，会话保持可补片（不写墓碑）。
#[tokio::test]
async fn s3_complete_missing_parts_returns_409_and_stays_repairable() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_903).await;
    rig.backend.set_list(vec![part(1), part(3)]);
    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], 20615);
    assert!(session.completed_file_id().expect("read tombstone").is_none());
}

/// §8.5 第 3 步分支二：HEAD 命中本 session + 摘要一致 → 补建行 + 幂等 abort。
#[tokio::test]
async fn s3_complete_head_hit_matching_sha_reuses_and_aborts_mpu() {
    const UPLOADER: u64 = 9_974_102;
    const FILE_ID: u64 = 9_974_102_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
    rig.probe.set_head(Some(upload_id_of(&token)), TOTAL_SIZE);
    rig.probe.set_sha256(&sealed_of(FILE_ID));

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.abort_calls(), 1, "复用前必须幂等 abort 当前 MPU");
    assert!(rig.backend.complete_call_parts().is_empty(), "HEAD 命中不该再走 Complete");

    cleanup(&rig.pool, UPLOADER).await.expect("用例后清库必须成功");
}

/// §8.5 第 3 步分支一：metadata 不属于本 session → 保留对象 + abort + 500。
/// 🔴 不得回 20618（重申请仍是同一 final_key，死循环），也不得删除。
#[tokio::test]
async fn s3_complete_head_hit_foreign_keeps_object_with_500() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_904).await;
    rig.probe.set_head(Some("another-session"), TOTAL_SIZE);

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error(), "身份不明必须 500：{json}");
    assert_ne!(json["code"], 20618, "绝不回 20618 让客户端死循环");
    assert_eq!(rig.probe.delete_calls(), 0, "无权删除：不得动已有对象");
    assert_eq!(rig.backend.abort_calls(), 1);
}

/// §8.5 第 3 步分支三：属于本 session + 摘要不一致 → 删除成功回 20618；
/// 删除失败保留会话回可重试 5xx（重试可自愈）。
#[tokio::test]
async fn s3_complete_head_hit_sha_mismatch_deletes_then_restarts_or_retries() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_905).await;
    rig.probe.set_head(Some(upload_id_of(&token)), TOTAL_SIZE);
    rig.probe.set_sha256(&"ff".repeat(32));

    // 删除成功 → 20618 RestartUpload。
    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json["code"], 20618);
    assert_eq!(rig.probe.delete_calls(), 1);

    // 删除失败 → 可重试 5xx，会话保留（墓碑不在）。
    rig.probe.fail_delete();
    let (status, _) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error());
    assert!(ChunkedSession::open(
        &rig.state.file_service.upload_session_root().unwrap(),
        &token
    )
    .is_ok());
}

/// 🔴 complete 分流对照（第十三轮冻结）：409 Conflict → 20618 从零重来。
#[tokio::test]
async fn s3_complete_conflict_maps_to_restart_upload() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_906).await;
    rig.backend.set_complete_result(NumberedPartError::Conflict);
    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json["code"], 20618, "409 = MPU 作废：回 20618");
    assert_eq!(rig.probe.delete_calls(), 0, "409 分支不删任何东西");
}

/// 🔴 412 PreconditionFailed → 回读已有对象核验身份：一致复用（先幂等 abort
/// 再建行），绝不删除 final key；不一致保留对象报 500。
#[tokio::test]
async fn s3_complete_precondition_failed_verifies_existing_object() {
    const UPLOADER: u64 = 9_974_103;
    const FILE_ID: u64 = 9_974_103_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
    rig.backend.set_complete_result(NumberedPartError::PreconditionFailed);
    // 第 3 步 HEAD 为空，412 恢复时的 HEAD 才命中本 session 对象。
    rig.probe.queue_head_none();
    rig.probe.queue_head(Some(upload_id_of(&token)), TOTAL_SIZE);
    rig.probe.set_sha256(&sealed_of(FILE_ID));

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "412 + 身份一致必须复用建行：{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.abort_calls(), 1, "建行前必须幂等 abort 当前 MPU");
    assert_eq!(rig.probe.delete_calls(), 0, "412 绝不删除 final key");
    cleanup(&rig.pool, UPLOADER).await.expect("用例后清库必须成功");

    // 身份不一致 → 保留对象、abort、500。
    let rig2 = make_rig().await;
    let (_, token2) = create_s3_session(&rig2, 9_974_200, 9_974_907).await;
    rig2.backend.set_complete_result(NumberedPartError::PreconditionFailed);
    rig2.probe.queue_head_none();
    rig2.probe.queue_head(Some("another-session"), TOTAL_SIZE);
    let (status, json) = call(&rig2, post_complete(&token2)).await;
    assert!(status.is_server_error(), "{json}");
    assert_eq!(rig2.probe.delete_calls(), 0);
    assert_eq!(rig2.backend.abort_calls(), 1);
}

/// §8.5 第 6 步：回读摘要不符 → 删成功回 20618；删失败保留会话回可重试 5xx。
#[tokio::test]
async fn s3_complete_readback_mismatch_deletes_then_restarts_or_retries() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_908).await;
    rig.probe.set_sha256(&"ee".repeat(32));

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json["code"], 20618, "回读不符必须 RestartUpload，禁止分片级 422 死循环");
    assert_eq!(rig.probe.delete_calls(), 1);
    assert!(session.completed_file_id().unwrap().is_none());

    rig.probe.fail_delete();
    let (status, _) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error(), "删失败必须可重试 5xx，会话保留");
    assert!(session.completed_file_id().unwrap().is_none());
}

/// 秒传命中：同摘要已存在 → 冗余 final 对象被删（归属已证明）→ 用既有路径建行。
#[tokio::test]
async fn s3_complete_dedup_hit_deletes_redundant_object_and_reuses_existing_path() {
    const UPLOADER: u64 = 9_974_104;
    const FILE_ID: u64 = 9_974_104_01;
    const EXISTING_FILE_ID: i64 = 9_974_104_02;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    // 预置行的 uploader 是别人，用例间重跑也得先把它清掉（真库共享）。
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(EXISTING_FILE_ID)
        .execute(&*rig.pool)
        .await
        .expect("清理预置行残留");
    // 预置一条同摘要的既有行（别人先传的同一份字节）。
    sqlx::query(
        "INSERT INTO privchat_file_uploads (file_id, original_filename, file_size, file_type, \
         mime_type, file_path, storage_source_id, uploader_id, uploaded_at, file_hash, \
         encryption_version) \
         VALUES ($1, 'old.bin', $2, 'file', 'application/octet-stream', 'files/old.bin', 0, $3, \
         extract(epoch from now())::bigint * 1000, $4, 0)",
    )
    .bind(EXISTING_FILE_ID)
    .bind(TOTAL_SIZE as i64)
    .bind(9_999_999i64)
    .bind(sealed_of(FILE_ID))
    .execute(&*rig.pool)
    .await
    .expect("预置既有行");

    let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
    // 第 3 步 HEAD 为空；秒传判重后删冗余对象前的归属核对 HEAD 命中。
    rig.probe.queue_head_none();
    rig.probe.queue_head(Some(upload_id_of(&token)), TOTAL_SIZE);
    rig.probe.set_sha256(&sealed_of(FILE_ID));
    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.probe.delete_calls(), 1, "冗余 final 对象必须被删（归属已证明）");
    // 新行指向既有物理路径（秒传复用），不是 final_key。
    let path: String =
        sqlx::query_scalar("SELECT file_path FROM privchat_file_uploads WHERE file_id = $1")
            .bind(FILE_ID as i64)
            .fetch_one(&*rig.pool)
            .await
            .expect("新行");
    assert_eq!(path, "files/old.bin");
    cleanup(&rig.pool, UPLOADER).await.expect("用例后清库必须成功");
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(EXISTING_FILE_ID)
        .execute(&*rig.pool)
        .await
        .expect("用例后清理预置行");
}

// ================= abort =================

/// 顺序冻结：abort → ListParts 确认清空 → 才删本地目录；确认失败会话保留。
#[tokio::test]
async fn s3_abort_confirms_cleanup_before_discarding_directory() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_909).await;
    let dir = session.dir().to_path_buf();

    // 第一次 ListParts 仍有 part（in-flight 写入）→ 再次 abort → 第二次确认空。
    rig.backend.queue_list(Ok(vec![part(1)]));
    rig.backend.queue_list(Ok(vec![]));
    let (status, json) = call(&rig, post_abort(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["aborted"], true);
    assert_eq!(rig.backend.abort_calls(), 2, "仍有 part 必须继续 abort");
    assert!(!dir.exists(), "确认清空后目录必须删除");

    // abort 失败 → 会话保留可重试，绝不先置本地终态。
    let rig2 = make_rig().await;
    let (session2, token2) = create_s3_session(&rig2, 9_974_200, 9_974_910).await;
    let dir2 = session2.dir().to_path_buf();
    rig2.backend.set_abort_result(NumberedPartError::Backend("s3 down".into()));
    let (status, _) = call(&rig2, post_abort(&token2)).await;
    assert!(status.is_server_error());
    assert!(dir2.exists(), "S3 调用失败时目录必须保留");
}

// ================= 串用守卫 =================

/// S3 会话调 /files/chunk → 20616 终局（端点与 transport 强绑定，§8.3）。
#[tokio::test]
async fn s3_session_calling_chunk_gets_upload_mode_conflict() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_911).await;
    let req = Request::builder()
        .method("PUT")
        .uri("/api/app/files/chunk?offset=0")
        .header("X-Upload-Token", &token)
        .header("X-Chunk-SHA256", "0".repeat(64))
        .body(Body::from(vec![0u8; 16]))
        .expect("build request");
    let (status, json) = call(&rig, req).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], 20616, "串用即终局：S3 会话绝不落本地 part");
}
