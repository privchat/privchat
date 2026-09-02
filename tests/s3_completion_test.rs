//! S3 分流门禁（RESUMABLE_UPLOAD_SPEC §8.3/§8.5，实现顺序第 3 步）。
//!
//! 用 fake 分片后端 + fake 对象探测覆盖 status/complete/abort 的 S3 分支：
//! complete 全程（HEAD 三分支 → 缺片预检 → 409/412 恢复 → 回读 → 建行）、
//! status 的 ListParts 换算与 NoSuchUpload HEAD 恢复、abort 的确认后才删目录，
//! 以及 S3 会话串用 /files/chunk 的 20616 终局。
//! 第十五轮评审新增：扫描器按 transport 分流的恢复/保留门禁、存储源冻结与
//! 默认源切换门禁、条件删除（ETag）拒绝门禁。
//! 第三十轮评审新增：同一会话重复 complete 的回归断言——不重新覆盖已完成
//! 对象，也不生成重复/错误的文件记录。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::FutureExt as _;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;

use privchat::config::{AttachmentKeys, FileStorageSourceConfig};
use privchat_protocol::attachment_crypto as ac;
use privchat::http::FileServerState;
use privchat::service::chunked_upload::{self, ChunkedSession, NewSession, S3Anchor};
use privchat::service::file_service::FileService;
use privchat::service::final_object_probe::{FinalObjectHead, FinalObjectProbe, ProbeError};
use privchat::service::numbered_parts::{
    CompletedPart, ListedPart, NumberedPartBackend, NumberedPartError, UploadReference,
};
use privchat::service::upload_token_service::UploadTokenService;

const PART_SIZE: u64 = 4 << 20;
/// 明文 9 MiB → 密文略大于 9 MiB，按 4 MiB 分片正好 3 片（4 + 4 + 约 1 MiB）。
const PLAIN_SIZE: u64 = 9 << 20;
const TOTAL_PARTS: u32 = 3;
const CHUNK_PLAIN_SIZE: u32 = ac::DEFAULT_CHUNK_PLAIN_SIZE;
const KEY_ID: u8 = 1;

/// 🔴 **用例喂给探测的必须是真密文**，不是一个假摘要。
///
/// 这些用例是"S3 三条发布路径都会解密重算身份"的门禁。探测只交出一个摘要字符串
/// 时，服务端拿什么校验都无从谈起——上一版正是这样，于是"回读摘要一致"这件事
/// 被测得严严实实，而"这串字节是不是 token 声明的那份内容"一条都没验到。
fn site_key() -> [u8; 32] {
    [0x5au8; 32]
}

fn site_key_b64() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(site_key())
}

/// 密文总长度（= 会话冻结的 `total_size`）。明文定长，所以每个用例都一样。
fn total_size() -> u64 {
    ac::sealed_len(PLAIN_SIZE, CHUNK_PLAIN_SIZE).expect("sealed_len")
}

/// 每个用例独立的明文：真库共享，同内容会在 converge_upload 里互相判重。
fn plain_of(file_id: u64) -> Vec<u8> {
    let mut v = vec![0u8; PLAIN_SIZE as usize];
    v[..8].copy_from_slice(&file_id.to_le_bytes());
    v
}

/// 真实封装出来的密文。9 MiB 的 AES-GCM 不便宜，按 file_id 缓存，一个用例里
/// 反复取也只封一次。
fn blob_of(file_id: u64) -> Arc<Vec<u8>> {
    static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<u64, Arc<Vec<u8>>>>> =
        std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some(hit) = cache.lock().unwrap().get(&file_id) {
        return hit.clone();
    }
    let blob = Arc::new(
        ac::encrypt_attachment_with_chunk_size(
            &plain_of(file_id),
            &site_key(),
            KEY_ID,
            CHUNK_PLAIN_SIZE,
        )
        .expect("封装测试密文"),
    );
    cache.lock().unwrap().insert(file_id, blob.clone());
    blob
}

/// **另一个文件**的合法密文。
///
/// 🔴 这就是攻击形状本身：声明 A 的身份，交上 B 的密文。两份密文各自完全合法、
/// 密文摘要也各自自洽——只有解密重算明文摘要才拦得住。用它替代上一版的
/// `"ff".repeat(32)`：那种假摘要只能测出"摘要不等"，测不出"身份不符"。
fn foreign_blob_of(file_id: u64) -> Arc<Vec<u8>> {
    blob_of(file_id.wrapping_add(777))
}

fn plaintext_sha256_of(file_id: u64) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(plain_of(file_id)))
}

/// 每个会话独立的 final key。
///
/// 🔴 不能所有用例共用一个路径：`privchat_attachment_objects` 对
/// `(storage_source_id, file_path)` 有唯一索引，共用路径会让第二个用例撞上第一个
/// 留下的对象行，表现成莫名其妙的"秒传命中"。生产里 final_key 由预留 file_id
/// 生成，本来就是一上传一个。
fn final_key_of(reserved_file_id: u64) -> String {
    format!("files/s3-payload-{reserved_file_id}.bin")
}

/// 密文摘要 = 服务端回读整个对象算出来的那个（落进对象行的 `sealed_sha256`）。
fn sealed_of(file_id: u64) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(blob_of(file_id).as_slice()))
}

fn size_of(n: u32) -> u64 {
    if n == TOTAL_PARTS {
        total_size() - PART_SIZE * (TOTAL_PARTS as u64 - 1)
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
    /// Complete 尝试次数（含失败；第三十轮重复 complete 回归用）。
    complete_attempts: AtomicU32,
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
            complete_attempts: AtomicU32::new(0),
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
    fn complete_attempts(&self) -> u32 {
        self.complete_attempts.load(Ordering::SeqCst)
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
        self.complete_attempts.fetch_add(1, Ordering::SeqCst);
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

/// HEAD 回包携带的 ETag：delete_if_match 以它为条件。
const FINAL_ETAG: &str = "etag-final-v1";

struct FakeProbe {
    head_queue: Mutex<VecDeque<Result<Option<FinalObjectHead>, ProbeError>>>,
    head: Mutex<Result<Option<FinalObjectHead>, ProbeError>>,
    /// final key 上那个对象的**真实字节**（`open_stream` 交出去的东西）。
    /// `Err` = 建流失败（连不上 / 超时 / 缺 Content-Length），必须可重试。
    object: Mutex<Result<Vec<u8>, ProbeError>>,
    delete_result: Mutex<Result<(), ProbeError>>,
    delete_calls: AtomicU32,
    /// 对象当前真实 ETag：与入参不符即拒绝删除（模拟 HEAD 与删除之间对象被替换）。
    current_etag: Mutex<String>,
}

impl FakeProbe {
    fn new() -> Self {
        Self {
            head_queue: Mutex::new(VecDeque::new()),
            head: Mutex::new(Ok(None)),
            object: Mutex::new(Ok(Vec::new())),
            delete_result: Mutex::new(Ok(())),
            delete_calls: AtomicU32::new(0),
            current_etag: Mutex::new(FINAL_ETAG.to_string()),
        }
    }
    /// 按调用次序下发的 HEAD 结果（用完回落到 `set_head` 的默认值）：
    /// 412/秒传用例需要「第一次 HEAD 空、后续命中」的时序。
    fn queue_head(&self, upload_id: Option<&str>, content_length: u64) {
        self.head_queue.lock().unwrap().push_back(Ok(Some(FinalObjectHead {
            content_length,
            privchat_upload_id: upload_id.map(|s| s.to_string()),
            etag: FINAL_ETAG.to_string(),
        })));
        *self.current_etag.lock().unwrap() = FINAL_ETAG.to_string();
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
            etag: FINAL_ETAG.to_string(),
        }));
        *self.current_etag.lock().unwrap() = FINAL_ETAG.to_string();
    }
    /// final key 上放一份**真密文**。
    fn set_object(&self, bytes: &[u8]) {
        *self.object.lock().unwrap() = Ok(bytes.to_vec());
    }
    /// 建流失败：连不上 / 超时 / 响应缺 Content-Length。字节可能好好地躺在桶里，
    /// 所以这一类必须可重试，而且**绝不允许**顺手删对象。
    fn fail_open_stream(&self) {
        *self.object.lock().unwrap() = Err(ProbeError::Backend("GET 建流失败".into()));
    }
    /// 模拟 HEAD 之后对象被替换（TOCTOU）：HEAD 快照仍带旧 ETag，
    /// delete_if_match 必须因条件不符而拒绝。
    fn mutate_etag(&self, new_etag: &str) {
        *self.current_etag.lock().unwrap() = new_etag.to_string();
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
    async fn open_stream(
        &self,
        _reference: &UploadReference,
    ) -> Result<(u64, std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send + Unpin>>), ProbeError> {
        let bytes = self.object.lock().unwrap().clone()?;
        // 长度与字节来自同一次"响应"，口径同真实后端。
        Ok((bytes.len() as u64, Box::pin(std::io::Cursor::new(bytes))))
    }
    async fn delete_if_match(
        &self,
        _reference: &UploadReference,
        etag: &str,
    ) -> Result<bool, ProbeError> {
        // 🔴 条件不符 = 对象已变化：拒绝删除，绝不动对象。
        if etag != *self.current_etag.lock().unwrap() {
            return Ok(false);
        }
        self.delete_result.lock().unwrap().clone().map(|()| {
            self.delete_calls.fetch_add(1, Ordering::SeqCst);
            true
        })
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
        direct_upload: None,
        region: None,
        addressing_style: None,
    };
    // 🔴 S3 存储源 id=1（第十五轮评审 P0 门禁）：default 仍是 0，建行必须指向
    // manifest 冻结的 id=1 而不是当前默认；桶与测试 manifest 的 bucket 一致。
    let s3_source = FileStorageSourceConfig {
        id: 1,
        storage_type: "s3".to_string(),
        storage_root: String::new(),
        base_url: Some("https://cdn.e2e.local/privchat-e2e".to_string()),
        endpoint: Some("http://s3.e2e.local".to_string()),
        bucket: Some("privchat-e2e".to_string()),
        access_key_id: Some("dummy-ak".to_string()),
        secret_access_key: Some("dummy-sk".to_string()),
        path_prefix: None,
        direct_upload: None,
        region: None,
        addressing_style: None,
    };
    let file_service = FileService::new(vec![source, s3_source], 0, pool.clone());
    file_service.init().await.expect("init storage");
    let backend = Arc::new(FakeBackend::new());
    let probe = Arc::new(FakeProbe::new());
    Rig {
        state: FileServerState {
            file_service: Arc::new(file_service),
            upload_token_service: Arc::new(UploadTokenService::new()),
            auth: None,
            // 🔴 校验要解密：没有这把密钥，三条发布路径全部 fail-closed。
            attachment_keys: AttachmentKeys(vec![(KEY_ID, site_key_b64())]),
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
    // 🔴 对象行没有 uploader，按 uploader 清不到；留着会让下一个用例撞上
    // `(storage_source_id, file_path)` 唯一索引，或者以同摘要判成秒传命中。
    sqlx::query(
        "DELETE FROM privchat_attachment_objects o WHERE NOT EXISTS \
         (SELECT 1 FROM privchat_file_uploads u WHERE u.object_id = o.object_id)",
    )
    .execute(pool)
    .await
    .map_err(|e| format!("cleanup 清理孤儿对象行失败: {e}"))?;
    Ok(())
}

/// 预置一条**已发布**的记录（对象行 + 文件行），模拟"别人先传过"或"崩溃窗口里
/// PG 已提交"。口径同生产：文件行只引用对象行，身份字段都在对象行上。
async fn seed_published_object(
    pool: &PgPool,
    file_id: i64,
    uploader: i64,
    file_path: &str,
    storage_source_id: i32,
    plaintext_sha256: &str,
    sealed_sha256: &str,
) {
    let object_id: i64 = sqlx::query_scalar(
        "INSERT INTO privchat_attachment_objects \
         (plaintext_sha256, plaintext_size, sealed_sha256, sealed_size, file_path, \
          storage_source_id, format_version, encryption_key_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING object_id",
    )
    .bind(plaintext_sha256)
    .bind(PLAIN_SIZE as i64)
    .bind(sealed_sha256)
    .bind(total_size() as i64)
    .bind(file_path)
    .bind(storage_source_id)
    .bind(i16::from(ac::FORMAT_VERSION))
    .bind(i16::from(KEY_ID))
    .fetch_one(pool)
    .await
    .expect("预置对象行");
    sqlx::query(
        "INSERT INTO privchat_file_uploads \
         (file_id, original_filename, file_type, mime_type, object_id, uploader_id, business_type) \
         VALUES ($1, 'seed.bin', 'file', 'application/octet-stream', $2, $3, 'message')",
    )
    .bind(file_id)
    .bind(object_id)
    .bind(uploader)
    .execute(pool)
    .await
    .expect("预置文件行");
}

/// 按 file_id 删行（预置行属于别的 uploader，`cleanup` 按 uploader 清不掉）。
async fn delete_by_file_id(pool: &PgPool, file_id: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
        .bind(file_id)
        .execute(pool)
        .await
        .map_err(|e| format!("DELETE file_id={file_id} 失败: {e}"))?;
    Ok(())
}

/// 🔴 真库卫生（第十五轮评审 P1）：失败路径也必须清库——catch_unwind 接住用例主体
/// （含断言 panic），先清掉本 uploader 的行，再把失败原样重新抛出。
async fn run_with_cleanup<F>(pool: &PgPool, uploader: u64, fut: F)
where
    F: std::future::Future<Output = ()>,
{
    let outcome = std::panic::AssertUnwindSafe(fut).catch_unwind().await;
    if let Err(payload) = outcome {
        if let Err(e) = cleanup(pool, uploader).await {
            eprintln!("[s3_completion] 用例后清库失败（真库可能已被污染）: {e}");
        }
        std::panic::resume_unwind(payload);
    }
    cleanup(pool, uploader)
        .await
        .expect("用例后清库必须成功，否则真库污染会在绿灯下持续累积");
}

/// 建 S3 会话并把 manifest 改写成建好 MPU 的形态（平铺三字段，§8.7）。
async fn create_s3_session(rig: &Rig, uploader: u64, reserved_file_id: u64) -> (ChunkedSession, String) {
    create_s3_session_full(
        rig,
        uploader,
        reserved_file_id,
        Some(1),
        "privchat-e2e",
    )
    .await
}

/// 全参版：`source_id=None` 模拟缺冻结字段的旧/坏 manifest；`bucket` 用来造
/// 「冻结源 bucket 与 manifest 不符」的负例（第十五轮评审 P0 门禁）。
async fn create_s3_session_full(
    rig: &Rig,
    uploader: u64,
    reserved_file_id: u64,
    source_id: Option<u32>,
    bucket: &str,
) -> (ChunkedSession, String) {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let (session, token, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: uploader,
            total_size: total_size(),
            // 🔴 token 冻结的身份：complete 只核对它，不从请求体里读。
            plaintext_sha256: plaintext_sha256_of(reserved_file_id),
            plaintext_size: PLAIN_SIZE,
            format_version: ac::FORMAT_VERSION,
            encryption_key_id: KEY_ID,
            chunk_plain_size: CHUNK_PLAIN_SIZE,
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "payload.bin".into(),
            mime_type: "application/octet-stream".into(),
            reserved_file_id,
            transport: "s3_multipart_v1".to_string(),
            s3: None,
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
    obj.insert("bucket".into(), serde_json::json!(bucket));
    obj.insert("final_key".into(), serde_json::json!(final_key_of(reserved_file_id)));
    obj.insert("provider_upload_id".into(), serde_json::json!("mpu-abc-123"));
    // 🔴 第二十九轮：模拟 part-url 签发时持久化的逐片摘要声明（与 part(n) 的
    // checksum 同源）——Complete 体的逐片 checksum 取自这里而不是 ListParts 回读。
    obj.insert(
        "part_digests".into(),
        serde_json::json!({
            "1": "checksum-1",
            "2": "checksum-2",
            "3": "checksum-3",
        }),
    );
    if let Some(id) = source_id {
        obj.insert("storage_source_id".into(), serde_json::json!(id));
    }
    std::fs::write(&manifest_path, manifest.to_string()).expect("rewrite manifest");
    (session, token)
}

/// 把会话 manifest 改成已过期（`expires_at=1`），返回目录路径（扫描器门禁用）。
fn expire_session(rig: &Rig, token: &str) -> std::path::PathBuf {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let dir = root.join("chunked").join(upload_id_of(token));
    let manifest_path = dir.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest
        .as_object_mut()
        .expect("object")
        .insert("expires_at".into(), serde_json::json!(1));
    std::fs::write(&manifest_path, manifest.to_string()).expect("rewrite manifest");
    dir
}

/// 跑一次 S3 扫描（fake 后端/探测已接线）。
async fn sweep_s3(rig: &Rig) -> usize {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let backend: Arc<dyn NumberedPartBackend> = rig.backend.clone();
    let probe: Arc<dyn FinalObjectProbe> = rig.probe.clone();
    chunked_upload::sweep_expired_s3(&root, Some(&backend), Some(&probe), &rig.state.file_service)
        .await
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

/// 🔴 长度异常的片一律视为缺失（不进 received，§8.3），不得产出语义不明的区间——
/// 否则 status 报完整而 complete 永远过不去。第二十九轮：换算依据只有几何证据，
/// 不回逐片摘要的片（COS 形态）照样如实报告已收区间。
#[tokio::test]
async fn s3_status_treats_bad_geometry_as_missing_and_ignores_checksum_absence() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_901).await;
    let mut bad_size = part(2);
    bad_size.size = PART_SIZE - 1; // 非末片长度异常
    let mut no_checksum = part(3);
    no_checksum.checksum_sha256_b64 = None; // COS 形态：ListParts 不回逐片摘要
    rig.backend.set_list(vec![part(1), bad_size, no_checksum]);
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let data = &json["data"];
    assert_eq!(data["received"].as_array().unwrap().len(), 2, "第 1、3 片几何合法：{json}");
    assert_eq!(data["received_bytes"], PART_SIZE + size_of(3));
    assert_eq!(data["missing"][0]["offset"], PART_SIZE);
    assert_eq!(data["missing"][0]["length"], PART_SIZE);
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
    rig.probe.set_head(Some(&upload_id), total_size());
    let (status, json) = call(&rig, get_status(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let data = &json["data"];
    assert_eq!(data["completed"], false, "status 不建行不写墓碑，completed 必须 false");
    assert_eq!(data["missing"].as_array().unwrap().len(), 0);
    assert_eq!(data["received"][0]["offset"], 0);
    assert_eq!(data["received"][0]["length"], total_size());

    // 长度不符 → 可重试 5xx，绝不把不完整对象报成完整。
    rig.probe.set_head(Some(&upload_id), total_size() - 1);
    let (status, _) = call(&rig, get_status(&token)).await;
    assert!(status.is_server_error(), "长度不符必须回可重试 5xx");

    // metadata 不属于本 session → 500 保留对象。
    rig.probe.set_head(Some("another-session"), total_size());
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
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, happy_path_core(&rig)).await;
}

async fn happy_path_core(rig: &Rig) {
    const UPLOADER: u64 = 9_974_101;
    const FILE_ID: u64 = 9_974_101_01;
    let (_, token) = create_s3_session(rig, UPLOADER, FILE_ID).await;
    // 回读摘要 = 会话声明的 sealed 才算身份一致（每个用例独立摘要）。
    rig.probe.set_object(blob_of(FILE_ID).as_slice());

    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "全链路必须成功：{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);

    // PG 行已建：身份字段都在对象行上——sealed 摘要 = 服务端回读算出来的那个，
    // 明文摘要 = token 冻结的那个（解密重算核对过），file_path = manifest 的 final_key。
    let row: Option<(String, String, String, i32)> = sqlx::query_as(
        "SELECT o.file_path, o.sealed_sha256, o.plaintext_sha256, o.storage_source_id \
         FROM privchat_file_uploads u \
         JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
         WHERE u.file_id = $1",
    )
    .bind(FILE_ID as i64)
    .fetch_optional(&*rig.pool)
    .await
    .expect("query row");
    let (path, hash, plaintext_hash, source_id) = row.expect("PG 行必须存在");
    assert_eq!(path, final_key_of(FILE_ID));
    assert_eq!(hash, sealed_of(FILE_ID));
    assert_eq!(plaintext_hash, plaintext_sha256_of(FILE_ID), "落库的明文身份必须是校验过的那个");
    // 🔴 默认源切换门禁（第十五轮评审 P0）：默认存储源是 id=0（local），
    // 行必须指向 manifest 冻结的 id=1，而不是当前默认。
    assert_eq!(source_id, 1, "建行必须指向冻结的存储源，而不是当前默认源");

    // Complete 提交的快照：ETag 来自 ListParts，逐片 checksum 来自 manifest 声明（第二十九轮）。
    let calls = rig.backend.complete_call_parts();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].len(), TOTAL_PARTS as usize);
    assert_eq!(calls[0][0].etag, "etag-1");
    assert_eq!(calls[0][0].checksum_sha256_b64, "checksum-1");

    // 墓碑幂等：重复 complete 回同一 file_id，不再触达 backend。
    let before = rig.backend.complete_call_parts().len();
    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.complete_call_parts().len(), before, "墓碑后不得再调 backend");

    // 🔴 第三十轮回归：重复 complete 后 PG 仍只有一行（无重复/错误记录）。
    assert_eq!(file_row_count(rig, FILE_ID).await, 1);
}

/// PG 中该 file_id 的行数（第三十轮重复 complete 回归用）。
async fn file_row_count(rig: &Rig, file_id: u64) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM privchat_file_uploads WHERE file_id = $1")
        .bind(file_id as i64)
        .fetch_one(&*rig.pool)
        .await
        .expect("count rows")
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

/// 🔴 第二十九轮核心回归：COS 形态——ListParts 不回逐片摘要（全部 None）。
/// status 按几何如实报告已收；complete 用 manifest 声明组装照走通，
/// Complete 体的逐片 checksum 与声明同源。
#[tokio::test]
async fn s3_complete_works_when_list_parts_returns_no_checksums() {
    const UPLOADER: u64 = 9_974_108;
    const FILE_ID: u64 = 9_974_108_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        // COS 形态：逐片摘要全空。
        let cos_parts: Vec<ListedPart> = all_parts()
            .into_iter()
            .map(|mut p| {
                p.checksum_sha256_b64 = None;
                p
            })
            .collect();
        rig.backend.set_list(cos_parts);
        rig.probe.set_object(blob_of(FILE_ID).as_slice());

        // status：几何证据照样报完整（判据 12/26 口径不变）。
        let (status, json) = call(&rig, get_status(&token)).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["data"]["received_bytes"], total_size());
        assert_eq!(json["data"]["missing"].as_array().unwrap().len(), 0);

        // complete：manifest 声明组装 → 回读一致 → 建行。
        let (status, json) = call(&rig, post_complete(&token)).await;
        assert_eq!(status, StatusCode::OK, "COS 形态下 complete 必须成功：{json}");
        assert_eq!(json["data"]["file_id"], FILE_ID);
        let calls = rig.backend.complete_call_parts();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].len(), TOTAL_PARTS as usize);
        assert_eq!(calls[0][0].etag, "etag-1", "ETag 仍取自 ListParts");
        assert_eq!(calls[0][0].checksum_sha256_b64, "checksum-1", "checksum 取自 manifest 声明");
    })
    .await;
}

/// 🔴 第二十九轮：几何齐但 manifest 缺某片声明 → 按缺失处理回 409，
/// 绝不拿空声明组装 Complete。
#[tokio::test]
async fn s3_complete_returns_409_when_manifest_lacks_part_declaration() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_930).await;
    // 把第 2 片声明从 manifest 里抠掉（模拟升级前签发的在途会话，§8.4 升级边界）。
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let manifest_path = root
        .join("chunked")
        .join(upload_id_of(&token))
        .join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read")).expect("parse");
    manifest["part_digests"].as_object_mut().expect("part_digests").remove("2");
    std::fs::write(&manifest_path, manifest.to_string()).expect("rewrite");

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::CONFLICT, "缺声明按缺片处理：{json}");
    assert_eq!(json["code"], 20615);
    assert_eq!(rig.backend.complete_call_parts().len(), 0, "不得发起 Complete");
}

/// §8.5 第 3 步分支二：HEAD 命中本 session + 摘要一致 → 补建行 + 幂等 abort。
#[tokio::test]
async fn s3_complete_head_hit_matching_sha_reuses_and_aborts_mpu() {
    const UPLOADER: u64 = 9_974_102;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, head_hit_matching_core(&rig)).await;
}

async fn head_hit_matching_core(rig: &Rig) {
    const UPLOADER: u64 = 9_974_102;
    const FILE_ID: u64 = 9_974_102_01;
    let (_, token) = create_s3_session(rig, UPLOADER, FILE_ID).await;
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());
    rig.probe.set_object(blob_of(FILE_ID).as_slice());

    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.abort_calls(), 1, "复用前必须幂等 abort 当前 MPU");
    assert!(rig.backend.complete_call_parts().is_empty(), "HEAD 命中不该再走 Complete");
}

/// §8.5 第 3 步分支一：metadata 不属于本 session → 保留对象 + abort + 500。
/// 🔴 不得回 20618（重申请仍是同一 final_key，死循环），也不得删除。
#[tokio::test]
async fn s3_complete_head_hit_foreign_keeps_object_with_500() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_904).await;
    rig.probe.set_head(Some("another-session"), total_size());

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
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());
    rig.probe.set_object(foreign_blob_of(9_974_905).as_slice());

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
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, precondition_failed_core(&rig)).await;
}

async fn precondition_failed_core(rig: &Rig) {
    const UPLOADER: u64 = 9_974_103;
    const FILE_ID: u64 = 9_974_103_01;
    let (_, token) = create_s3_session(rig, UPLOADER, FILE_ID).await;
    rig.backend.set_complete_result(NumberedPartError::PreconditionFailed);
    // 第 3 步 HEAD 为空，412 恢复时的 HEAD 才命中本 session 对象。
    rig.probe.queue_head_none();
    rig.probe.queue_head(Some(upload_id_of(&token)), total_size());
    rig.probe.set_object(blob_of(FILE_ID).as_slice());

    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "412 + 身份一致必须复用建行：{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.abort_calls(), 1, "建行前必须幂等 abort 当前 MPU");
    assert_eq!(rig.probe.delete_calls(), 0, "412 绝不删除 final key");

    // 🔴 第三十轮回归：同一会话重复 complete（412 恢复路径）不重新覆盖已完成对象、
    // 不生成重复/错误记录：Complete 只尝试过一次（412 即被后端拒绝，对象未被重写），
    // 恢复不再重发；PG 只有一行。
    assert_eq!(rig.backend.complete_attempts(), 1, "恢复不得再发 Complete（不覆盖对象）");
    assert_eq!(file_row_count(rig, FILE_ID).await, 1);

    // 恢复已写墓碑：第三次 complete 幂等回同一 file_id，仍不动对象、不加行。
    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.backend.complete_attempts(), 1);
    assert_eq!(rig.probe.delete_calls(), 0);
    assert_eq!(file_row_count(rig, FILE_ID).await, 1);

    // 身份不一致 → 保留对象、abort、500（不建行）。
    let rig2 = make_rig().await;
    let (_, token2) = create_s3_session(&rig2, 9_974_200, 9_974_907).await;
    rig2.backend.set_complete_result(NumberedPartError::PreconditionFailed);
    rig2.probe.queue_head_none();
    rig2.probe.queue_head(Some("another-session"), total_size());
    let (status, json) = call(&rig2, post_complete(&token2)).await;
    assert!(status.is_server_error(), "{json}");
    assert_eq!(rig2.probe.delete_calls(), 0);
    assert_eq!(rig2.backend.abort_calls(), 1);
}

/// §8.5 第 6 步：回读摘要不符 → 删成功回 20618；删失败保留会话回可重试 5xx。
/// 🔴 删除前先 HEAD 拿 ETag 作条件（防 TOCTOU）：第 3 步 HEAD 空，删除前
/// 的 HEAD 才命中本会话对象。
#[tokio::test]
async fn s3_complete_readback_mismatch_deletes_then_restarts_or_retries() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_908).await;
    rig.probe.set_object(foreign_blob_of(9_974_908).as_slice());
    rig.probe.queue_head_none(); // 第 3 步 HEAD 空，放行到 Complete + 回读
    rig.probe.set_head(Some(upload_id_of(&token)), total_size()); // 删除前 HEAD 命中本会话对象

    let (status, json) = call(&rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(json["code"], 20618, "回读不符必须 RestartUpload，禁止分片级 422 死循环");
    assert_eq!(rig.probe.delete_calls(), 1);
    assert!(session.completed_file_id().unwrap().is_none());

    // 重试时第 3 步 HEAD 命中（对象还在）→ 走三分支的删除分支，删失败回 5xx。
    rig.probe.fail_delete();
    let (status, _) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error(), "删失败必须可重试 5xx，会话保留");
    assert!(session.completed_file_id().unwrap().is_none());
}

/// 秒传命中：同摘要已存在 → 冗余 final 对象被删（归属已证明）→ 用既有路径建行。
#[tokio::test]
async fn s3_complete_dedup_hit_deletes_redundant_object_and_reuses_existing_path() {
    const UPLOADER: u64 = 9_974_104;
    const EXISTING_FILE_ID: i64 = 9_974_104_02;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    // 预置行的 uploader 是别人，用例间重跑也得先把它清掉（真库共享）。
    delete_by_file_id(&rig.pool, EXISTING_FILE_ID)
        .await
        .expect("清理预置行残留");
    run_with_cleanup(&rig.pool, UPLOADER, dedup_hit_core(&rig)).await;
    delete_by_file_id(&rig.pool, EXISTING_FILE_ID)
        .await
        .expect("用例后清理预置行");
}

async fn dedup_hit_core(rig: &Rig) {
    const UPLOADER: u64 = 9_974_104;
    const FILE_ID: u64 = 9_974_104_01;
    const EXISTING_FILE_ID: i64 = 9_974_104_02;
    // 预置一条同摘要的既有行（别人先传的同一份字节）。
    // 🔴 判重键是**明文**摘要：同一份内容被另一个用户先传过。
    seed_published_object(
        &rig.pool,
        EXISTING_FILE_ID,
        9_999_999,
        "files/old.bin",
        0,
        &plaintext_sha256_of(FILE_ID),
        &sealed_of(FILE_ID),
    )
    .await;

    let (_, token) = create_s3_session(rig, UPLOADER, FILE_ID).await;
    // 第 3 步 HEAD 为空；秒传判重后删冗余对象前的归属核对 HEAD 命中。
    rig.probe.queue_head_none();
    rig.probe.queue_head(Some(upload_id_of(&token)), total_size());
    rig.probe.set_object(blob_of(FILE_ID).as_slice());
    let (status, json) = call(rig, post_complete(&token)).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["file_id"], FILE_ID);
    assert_eq!(rig.probe.delete_calls(), 1, "冗余 final 对象必须被删（归属已证明）");
    // 新行指向既有物理路径（秒传复用），不是 final_key。
    let path: String = sqlx::query_scalar(
        "SELECT o.file_path FROM privchat_file_uploads u \
         JOIN privchat_attachment_objects o ON o.object_id = u.object_id WHERE u.file_id = $1",
    )
            .bind(FILE_ID as i64)
            .fetch_one(&*rig.pool)
            .await
            .expect("新行");
    assert_eq!(path, "files/old.bin");
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

// ================= 扫描器（第十五轮评审 P0） =================

/// abort 失败 → 保留目录下一轮重试：绝不先丢恢复信息。
#[tokio::test]
async fn sweep_s3_retains_dir_when_abort_fails() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_912).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_abort_result(NumberedPartError::Backend("s3 down".into()));

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "abort 失败：目录必须保留等下一轮重试");
    assert_eq!(rig.backend.abort_calls(), 1);
    assert_eq!(rig.probe.delete_calls(), 0);
}

/// HEAD 对象不存在 → MPU 已 abort 无残留，删目录。
#[tokio::test]
async fn sweep_s3_removes_dir_when_object_absent() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_913).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);

    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists());
    assert_eq!(rig.probe.delete_calls(), 0, "对象不在：不该进删除分支");
}

/// 对象归属外属 → 永不删对象，目录保留作人工排查锚点。
#[tokio::test]
async fn sweep_s3_retains_dir_for_foreign_object() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_914).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
    rig.probe.set_head(Some("another-session"), total_size());

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "外属对象：目录保留作人工排查锚点");
    assert_eq!(rig.probe.delete_calls(), 0, "无权删除：不得动外属对象");
}

/// 归属本属 + PG 已有身份一致的行（「PG 已提交、墓碑没写」崩溃窗口）：
/// 对象是正式数据，保留对象只删目录。
#[tokio::test]
async fn sweep_s3_keeps_object_when_pg_row_matches_identity() {
    const UPLOADER: u64 = 9_974_106;
    const FILE_ID: u64 = 9_974_106_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        let dir = expire_session(&rig, &token);
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
        // 模拟崩溃窗口：PG 行已提交、墓碑没写。
        // 崩溃窗口：PG 行已提交、墓碑没写。行指向的正是这次会话的 final key。
        seed_published_object(
            &rig.pool,
            FILE_ID as i64,
            UPLOADER as i64,
            &final_key_of(FILE_ID),
            1,
            &plaintext_sha256_of(FILE_ID),
            &sealed_of(FILE_ID),
        )
        .await;
        rig.probe.set_head(Some(upload_id_of(&token)), total_size());

        assert_eq!(sweep_s3(&rig).await, 1);
        assert!(!dir.exists(), "PG 行已是正式数据：目录可删");
        assert_eq!(rig.probe.delete_calls(), 0, "对象是正式数据：绝不删");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE file_id = $1")
                .bind(FILE_ID as i64)
                .fetch_one(&*rig.pool)
                .await
                .expect("复查行");
        assert_eq!(count, 1, "PG 行必须保留");
    })
    .await;
}

/// 归属本属 + 无 PG 行 → 冗余对象：条件删除成功后才删目录。
#[tokio::test]
async fn sweep_s3_deletes_orphan_object_via_conditional_delete() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_915).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());

    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists());
    assert_eq!(rig.probe.delete_calls(), 1, "冗余对象必须被条件删除");
}

/// 条件删除被拒（HEAD 后对象已变化，ETag 不符）→ 保留目录下一轮重新核验。
#[tokio::test]
async fn sweep_s3_retains_dir_when_conditional_delete_rejected() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_916).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());
    rig.probe.mutate_etag("etag-replaced"); // HEAD 之后对象被替换

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "删除被拒：目录必须保留等下一轮重新核验");
    assert_eq!(rig.probe.delete_calls(), 0, "ETag 不符绝不删");
}

/// 墓碑在 → complete 已终态，直接删目录（不再动对象侧）。
#[tokio::test]
async fn sweep_s3_removes_tombstoned_dir_without_backend_calls() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_917).await;
    session.write_completed(9_974_917_01).expect("写墓碑");
    let dir = expire_session(&rig, &token);

    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists());
    assert_eq!(rig.backend.abort_calls(), 0, "终态会话：不再动对象侧");
}

/// 🔴 墓碑在但锁被持有（第十六轮评审 P0）：complete 写完墓碑后可能仍持锁未返回，
/// 扫描器必须先拿非阻塞锁，拿不到就跳过——不得删掉进行中的完成流程。
#[tokio::test]
async fn sweep_s3_retains_tombstoned_dir_while_lock_is_held() {
    let rig = make_rig().await;
    let (session, token) = create_s3_session(&rig, 9_974_200, 9_974_922).await;
    session.write_completed(9_974_922_01).expect("写墓碑");
    let dir = expire_session(&rig, &token);
    // 模拟 complete 写完墓碑后仍持锁未返回。
    let held = session.try_lock().expect("lock io").expect("锁必须能拿到");

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "持锁中的完成流程：目录绝不删");
    assert!(dir.join("manifest.json").exists(), "锁后删不得碰会话文件");
    assert_eq!(rig.backend.abort_calls(), 0);

    // 锁释放后（complete 已返回）下一轮才允许删。
    drop(held);
    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists());
}

/// 🔴 abort 后 ListParts 确认（第十六轮评审 P1）：仍有残留 → 继续 abort，
/// 直到确认为空才放行删目录。
#[tokio::test]
async fn sweep_s3_keeps_aborting_until_list_parts_confirms_empty() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_923).await;
    let dir = expire_session(&rig, &token);
    // 前两轮确认后仍残留，第三轮才清空。
    rig.backend.queue_list(Ok(all_parts()));
    rig.backend.queue_list(Ok(all_parts()));
    rig.backend.set_list(vec![]);

    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists(), "确认清空后放行删目录");
    assert_eq!(rig.backend.abort_calls(), 3, "残留不清就继续 abort");
}

/// 反复 abort 后仍残留 → 保留目录下一轮重试，绝不在 parts 残留时删本地会话。
#[tokio::test]
async fn sweep_s3_retains_dir_when_parts_never_clear() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_924).await;
    let dir = expire_session(&rig, &token);
    rig.backend.set_list(all_parts()); // 确认后永远残留
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "残留不清：目录保留等下一轮");
    assert_eq!(rig.backend.abort_calls(), 3, "上限内尽力 abort");
    assert_eq!(rig.probe.delete_calls(), 0, "未确认清空不得进入删除分支");
}

/// manifest 损坏但锚点在（第十七轮评审 P2）：不盲删。锚点 GC 完成 abort 确认后
/// 对象不在 → 删锚点，随后主循环见无锚点才删目录；全程不丢恢复信息。
#[tokio::test]
async fn sweep_s3_recovers_corrupt_manifest_dir_via_anchor() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_925).await;
    let upload_id = upload_id_of(&token).to_string();
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let dir = root.join("chunked").join(&upload_id);
    // 断电写坏 manifest。
    std::fs::write(dir.join("manifest.json"), "{\"expires_at\": 1, \"torn").expect("corrupt");
    chunked_upload::write_s3_anchor(
        &root,
        &upload_id,
        &S3Anchor {
            bucket: "privchat-e2e".into(),
            final_key: "files/s3-payload.bin".into(),
            provider_upload_id: "mpu-abc-123".into(),
            created_at: 1,
        },
    )
    .expect("写锚点");
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
    // FakeProbe 默认 HEAD 空 = 无残留对象。

    // 锚点 GC 确认清空后直接收掉目录（不进主循环计数）。
    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(!dir.exists(), "锚点恢复完成后目录才允许删");
    assert!(rig.backend.abort_calls() >= 1, "凭锚点找回并 abort 了 MPU");
    assert!(
        !chunked_upload::s3_anchor_root(&root).join(format!("{upload_id}.json")).exists(),
        "锚点必须随之收掉"
    );
}

/// manifest 损坏 + 锚点在 + final 对象仍在（无论归属）：无 manifest 不能证明可删，
/// 锚点与目录都保留，人工排查；对象消失后的下一轮才放行。
#[tokio::test]
async fn sweep_s3_keeps_corrupt_manifest_dir_while_final_object_exists() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_926).await;
    let upload_id = upload_id_of(&token).to_string();
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let dir = root.join("chunked").join(&upload_id);
    std::fs::write(dir.join("manifest.json"), "garbage").expect("corrupt");
    chunked_upload::write_s3_anchor(
        &root,
        &upload_id,
        &S3Anchor {
            bucket: "privchat-e2e".into(),
            final_key: "files/s3-payload.bin".into(),
            provider_upload_id: "mpu-abc-123".into(),
            created_at: 1,
        },
    )
    .expect("写锚点");
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
    rig.probe.set_head(Some(&upload_id), total_size());

    assert_eq!(sweep_s3(&rig).await, 0);
    assert!(dir.exists(), "对象仍在：目录保留作人工排查锚点");
    assert!(
        chunked_upload::s3_anchor_root(&root).join(format!("{upload_id}.json")).exists(),
        "锚点保留：下一轮/人工处置的入口"
    );
    assert_eq!(rig.probe.delete_calls(), 0, "无 manifest 绝不删对象");
}

/// manifest 损坏且无锚点 → 可证 MPU 从未创建（不变式：create 成功必先写锚），
/// 与 proxy 同口径直接清。
#[tokio::test]
async fn sweep_s3_removes_corrupt_manifest_dir_without_anchor() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_927).await;
    let upload_id = upload_id_of(&token).to_string();
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let dir = root.join("chunked").join(&upload_id);
    std::fs::write(dir.join("manifest.json"), "").expect("corrupt");

    assert_eq!(sweep_s3(&rig).await, 1);
    assert!(!dir.exists());
    assert_eq!(rig.backend.abort_calls(), 0, "无锚点 = 无 MPU，不动对象侧");
}

/// 签发成功后的残留锚点（删锚失败留下的）：manifest 健在 → 锚点 GC 直接收，
/// 不碰会话目录。
#[tokio::test]
async fn sweep_s3_collects_stale_anchor_of_live_session() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_928).await;
    let upload_id = upload_id_of(&token).to_string();
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let dir = root.join("chunked").join(&upload_id);
    chunked_upload::write_s3_anchor(
        &root,
        &upload_id,
        &S3Anchor {
            bucket: "privchat-e2e".into(),
            final_key: "files/s3-payload.bin".into(),
            provider_upload_id: "mpu-abc-123".into(),
            created_at: 1,
        },
    )
    .expect("写锚点");
    rig.backend.set_list_err(NumberedPartError::NoSuchUpload);

    assert_eq!(sweep_s3(&rig).await, 0, "未过期会话不删");
    assert!(dir.exists());
    assert!(
        !chunked_upload::s3_anchor_root(&root).join(format!("{upload_id}.json")).exists(),
        "manifest 健在：残留锚点直接收掉"
    );
    assert_eq!(rig.backend.abort_calls(), 0, "健在会话的锚点不得触发 abort");
}

/// 未过期与 proxy 会话 → S3 扫描器不碰。
#[tokio::test]
async fn sweep_s3_skips_live_and_proxy_sessions() {
    let rig = make_rig().await;
    let root = rig.state.file_service.upload_session_root().expect("session root");

    // 未过期 S3 会话。
    let (_, live_token) = create_s3_session(&rig, 9_974_200, 9_974_918).await;
    let live_dir = root.join("chunked").join(upload_id_of(&live_token));

    // 过期 proxy 会话。
    let (proxy_session, _, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: 9_974_200,
            total_size: 16,
            // 扫描器用例不走校验，冻结身份填成形即可。
            plaintext_sha256: "0".repeat(64),
            plaintext_size: 16,
            format_version: ac::FORMAT_VERSION,
            encryption_key_id: KEY_ID,
            chunk_plain_size: CHUNK_PLAIN_SIZE,
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "p.bin".into(),
            mime_type: "application/octet-stream".into(),
            reserved_file_id: 9_974_919,
            transport: "proxy_offset_v1".into(),
            s3: None,
        },
    )
    .expect("建 proxy 会话");
    let proxy_dir = proxy_session.dir().to_path_buf();
    let manifest_path = proxy_dir.join("manifest.json");
    let mut m: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read")).expect("parse");
    m.as_object_mut()
        .expect("object")
        .insert("expires_at".into(), serde_json::json!(1));
    std::fs::write(&manifest_path, m.to_string()).expect("rewrite");

    assert_eq!(sweep_s3(&rig).await, 0, "未过期 / proxy 会话不归 S3 扫描器管");
    assert!(live_dir.exists());
    assert!(proxy_dir.exists());
}

/// 🔴 proxy 扫描器必须跳过过期的 S3 会话（否则恢复信息永久丢失）。
#[tokio::test]
async fn sweep_expired_proxy_skips_s3_sessions() {
    let rig = make_rig().await;
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let (_, s3_token) = create_s3_session(&rig, 9_974_200, 9_974_920).await;
    let s3_dir = expire_session(&rig, &s3_token);

    assert_eq!(chunked_upload::sweep_expired(&root), 0);
    assert!(s3_dir.exists(), "proxy 扫描器不得删 S3 会话目录");
}

/// 直传门禁未接入（后端/探测缺失）→ 保留目录记日志，绝不盲删。
#[tokio::test]
async fn sweep_s3_retains_dir_when_gate_not_wired() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_921).await;
    let dir = expire_session(&rig, &token);
    let root = rig.state.file_service.upload_session_root().expect("session root");

    let removed =
        chunked_upload::sweep_expired_s3(&root, None, None, &rig.state.file_service).await;
    assert_eq!(removed, 0);
    assert!(dir.exists(), "门禁未接入：目录保留，等人工排查/门禁接入");
}

// ================= 存储源冻结（第十五轮评审 P0） =================

/// 缺冻结的 storage_source_id → 500 拒绝建行，绝不回落当前默认源。
#[tokio::test]
async fn s3_complete_rejects_missing_frozen_storage_source() {
    const UPLOADER: u64 = 9_974_105;
    const FILE_ID: u64 = 9_974_105_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        let (_, token) = create_s3_session_full(
            &rig,
            UPLOADER,
            FILE_ID,
            None,
            "privchat-e2e",
        )
        .await;
        // 回读放行，必须死在冻结值校验上。
        rig.probe.set_object(blob_of(FILE_ID).as_slice());
        let (status, _) = call(&rig, post_complete(&token)).await;
        assert!(status.is_server_error(), "缺冻结存储源必须拒绝建行");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE file_id = $1")
                .bind(FILE_ID as i64)
                .fetch_one(&*rig.pool)
                .await
                .expect("查行");
        assert_eq!(count, 0, "不得建行");
    })
    .await;
}

/// 冻结源的 bucket 与 manifest bucket 不符 → 500 拒绝建行（防配置漂移后
/// PG 指向错误存储源）。
#[tokio::test]
async fn s3_complete_rejects_bucket_drift_against_frozen_source() {
    const UPLOADER: u64 = 9_974_107;
    const FILE_ID: u64 = 9_974_107_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        let (_, token) = create_s3_session_full(
            &rig,
            UPLOADER,
            FILE_ID,
            Some(1),
            "another-bucket",
        )
        .await;
        rig.probe.set_object(blob_of(FILE_ID).as_slice());
        let (status, _) = call(&rig, post_complete(&token)).await;
        assert!(status.is_server_error(), "bucket 不符必须拒绝建行");
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE file_id = $1")
                .bind(FILE_ID as i64)
                .fetch_one(&*rig.pool)
                .await
                .expect("查行");
        assert_eq!(count, 0, "不得建行");
    })
    .await;
}

// ================= 条件删除（第十五轮评审 P1） =================

/// HEAD → 删除的 TOCTOU：分支三删除前对象被替换（ETag 不符）必须拒绝删除，
/// 回可重试 5xx，绝不删错对象。
#[tokio::test]
async fn s3_complete_branch3_rejects_delete_when_etag_changed() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_922).await;
    rig.probe.set_head(Some(upload_id_of(&token)), total_size());
    rig.probe.set_object(foreign_blob_of(9_974_922).as_slice()); // 摘要不符 → 分支三进删除
    rig.probe.mutate_etag("etag-replaced");

    let (status, _) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error(), "删除被拒必须回可重试 5xx");
    assert_eq!(rig.probe.delete_calls(), 0, "ETag 不符绝不删");
    assert!(
        ChunkedSession::open(
            &rig.state.file_service.upload_session_root().unwrap(),
            &token
        )
        .is_ok(),
        "会话必须保留等重试"
    );
}

/// 回读不符路径（第 6 步）的同一道 TOCTOU 门禁。
#[tokio::test]
async fn s3_complete_readback_mismatch_rejects_delete_when_etag_changed() {
    let rig = make_rig().await;
    let (_, token) = create_s3_session(&rig, 9_974_200, 9_974_923).await;
    rig.probe.queue_head_none(); // 第 3 步 HEAD 空，放行到 Complete
    rig.probe.set_head(Some(upload_id_of(&token)), total_size()); // 第 6 步删除前 HEAD 命中
    rig.probe.set_object(foreign_blob_of(9_974_923).as_slice());
    rig.probe.mutate_etag("etag-replaced");

    let (status, _) = call(&rig, post_complete(&token)).await;
    assert!(status.is_server_error(), "删除被拒必须回可重试 5xx，不得 20618");
    assert_eq!(rig.probe.delete_calls(), 0, "ETag 不符绝不删");
}

// ================= 首传校验门禁（第三十一轮）：三条发布路径都必须解密重算身份 =================
//
// 🔴 这一组是「S3 三条发布路径真的在校验」的唯一证据。
//
// 它们喂的不是"摘要对不上的字节"，而是**另一个文件的完全合法的密文**：密文自洽、
// GCM tag 全部验得过、长度也一样。密文摘要那一层拦不住它——上一版的探测只交出一个
// 摘要字符串，于是这类攻击在测试里根本无法表达，"回读一致"测得再严也没验到身份。
//
// 漏掉任何一条路径，那条就是完整的绕过入口：任何登录用户都能用「文件 A 的摘要 +
// 文件 B 的密文」把 A 的秒传结果污染成 B。

/// 正常 complete：声明 A 的身份、桶里躺着 B 的合法密文 → 拒绝 + 删对象 + 不建行。
#[tokio::test]
async fn s3_complete_rejects_a_valid_ciphertext_that_is_not_the_declared_file() {
    const UPLOADER: u64 = 9_974_110;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        const FILE_ID: u64 = 9_974_110_01;
        let (session, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        rig.probe.queue_head_none(); // 第 3 步 HEAD 空，放行到 Complete + 回读
        rig.probe.set_head(Some(upload_id_of(&token)), total_size()); // 删除前 HEAD 命中本会话对象
        // 合法密文，但它是**另一个文件**的。
        rig.probe.set_object(foreign_blob_of(FILE_ID).as_slice());

        let (status, json) = call(&rig, post_complete(&token)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "身份不符必须拒绝: {json}");
        assert_eq!(json["code"], 20618, "拒绝后要从头上传，不能留在原会话死循环");
        assert_eq!(rig.probe.delete_calls(), 1, "归属已证明的坏对象必须删掉");
        assert_eq!(file_row_count(&rig, FILE_ID).await, 0, "🔴 未通过校验的字节绝不建行");
        assert!(
            session.completed_file_id().unwrap().is_none(),
            "没建行就不能写墓碑，否则重试会拿到一个不存在的 file_id"
        );
    })
    .await;
}

/// §8.5 第 3 步分支二（HEAD 命中本会话对象 → 复用建行）同样要校验：
/// 归属只回答"谁写的"，回答不了"写的是不是声明的那份"。
#[tokio::test]
async fn s3_complete_existing_object_recovery_still_verifies_identity() {
    const UPLOADER: u64 = 9_974_111;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        const FILE_ID: u64 = 9_974_111_01;
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        // 第 3 步 HEAD 就命中本会话对象 → 走恢复分支，不经过 Complete。
        rig.probe.set_head(Some(upload_id_of(&token)), total_size());
        rig.probe.set_object(foreign_blob_of(FILE_ID).as_slice());

        let (status, json) = call(&rig, post_complete(&token)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{json}");
        assert_eq!(json["code"], 20618);
        assert_eq!(rig.backend.complete_attempts(), 0, "恢复分支不发 Complete");
        assert_eq!(rig.probe.delete_calls(), 1, "归属已证明 → 删掉坏对象");
        assert_eq!(file_row_count(&rig, FILE_ID).await, 0, "🔴 未通过校验的字节绝不建行");
    })
    .await;
}

/// §8.5 第 5 步 412 恢复：身份不符 → 报冲突人工排查，**绝不删 final key**
/// （那上面是此前已存在的对象，不归本会话处置）。
#[tokio::test]
async fn s3_complete_412_recovery_rejects_foreign_identity_without_deleting() {
    const UPLOADER: u64 = 9_974_112;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        const FILE_ID: u64 = 9_974_112_01;
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        rig.backend.set_complete_result(NumberedPartError::PreconditionFailed);
        rig.probe.queue_head_none(); // 第 3 步 HEAD 空
        rig.probe.queue_head(Some(upload_id_of(&token)), total_size()); // 412 恢复时命中
        rig.probe.set_object(foreign_blob_of(FILE_ID).as_slice());

        let (status, _) = call(&rig, post_complete(&token)).await;
        assert!(status.is_server_error(), "412 + 身份不符 = 冲突，等人工排查");
        assert_eq!(rig.probe.delete_calls(), 0, "🔴 412 路径绝不删 final key");
        assert_eq!(rig.backend.abort_calls(), 1, "拒绝前必须 abort 当前 MPU");
        assert_eq!(file_row_count(&rig, FILE_ID).await, 0, "🔴 未通过校验的字节绝不建行");
    })
    .await;
}

/// 回读**建流失败**（连不上 / 超时 / 缺 Content-Length）：字节可能好好地躺在桶里。
/// 必须回可重试错误、保留会话、**绝不删对象**——把一次抖动判成内容错误，等于让
/// 一份完好的上传永久作废。
#[tokio::test]
async fn s3_complete_open_stream_failure_is_retryable_and_deletes_nothing() {
    const UPLOADER: u64 = 9_974_113;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        const FILE_ID: u64 = 9_974_113_01;
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        rig.probe.queue_head_none();
        rig.probe.set_head(Some(upload_id_of(&token)), total_size());
        rig.probe.fail_open_stream();

        let (status, json) = call(&rig, post_complete(&token)).await;
        // 🔴 锁死**具体**的可重试形态，不是"某种 5xx"。
        //
        // 只断言 is_server_error 的话，实现从 ServiceUnavailable 退回 Internal 照样绿——
        // 而 SDK 的可重试白名单并不覆盖所有 Internal，那正是"一次抖动把一份好好躺在
        // 桶里的对象永久废掉"的复发路径。
        assert_eq!(
            status,
            StatusCode::SERVICE_UNAVAILABLE,
            "存储读不动必须是可重试的 503，不是笼统的 5xx: {json}"
        );
        assert_eq!(json["code"], 3, "错误码必须是 ServiceUnavailable(3): {json}");
        assert_ne!(json["code"], 20618, "🔴 不得把一次读取失败判成「从头重传」");
        assert_eq!(rig.probe.delete_calls(), 0, "🔴 读不动就删对象 = 把抖动变成数据丢失");
        assert_eq!(file_row_count(&rig, FILE_ID).await, 0);
        assert!(
            ChunkedSession::open(
                &rig.state.file_service.upload_session_root().unwrap(),
                &token
            )
            .is_ok(),
            "会话必须保留等重试"
        );

        // 存储恢复之后，同一张 token 重试必须能走完（这才叫"可重试"）。
        rig.probe.set_object(blob_of(FILE_ID).as_slice());
        rig.probe.queue_head_none();
        let (status, json) = call(&rig, post_complete(&token)).await;
        assert_eq!(status, StatusCode::OK, "存储恢复后重试必须成功: {json}");
        assert_eq!(json["data"]["file_id"], FILE_ID);
    })
    .await;
}

/// 🔴 校验不符、但 final key 上的对象**已经不是本会话的**（校验与 HEAD 之间被替换）：
/// 一律不删。
///
/// ETag 条件删除只保证「删的是我 HEAD 到的那一个」，保证不了「那一个是我的」。
/// 少了归属核对，一个被替换进来的、别人的对象会被条件删除干净利落地删掉。
#[tokio::test]
async fn s3_complete_readback_mismatch_never_deletes_a_foreign_object() {
    const UPLOADER: u64 = 9_974_114;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        const FILE_ID: u64 = 9_974_114_01;
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        rig.probe.queue_head_none(); // 第 3 步 HEAD 空，放行到 Complete + 回读
        rig.probe.set_object(foreign_blob_of(FILE_ID).as_slice()); // 校验必然不过
        // 删除前的 HEAD：对象已被替换成别人的（metadata 是另一个会话）。
        rig.probe.set_head(Some("another-session"), total_size());

        let (status, json) = call(&rig, post_complete(&token)).await;
        assert!(status.is_server_error(), "归属证明不了 → 报冲突等人工排查: {json}");
        assert_ne!(json["code"], 20618, "重申请仍是同一 final_key，回 20618 会死循环");
        assert_eq!(rig.probe.delete_calls(), 0, "🔴 绝不删不属于本会话的对象");
        assert_eq!(rig.backend.abort_calls(), 1, "拒绝前必须 abort 当前 MPU");
        assert_eq!(file_row_count(&rig, FILE_ID).await, 0);
    })
    .await;
}

/// 🔴 秒传命中之后崩在墓碑之前：PG 行身份全对，但它引用的是**别人那份对象**。
///
/// 这条是路径比较的反例门禁。uploader、明文摘要、明文大小三项完全一致——只有
/// `file_path` 不同（行指向先传者的路径，而 final_key 上躺着本会话刚发布的那份
/// 冗余对象）。少了坐标比较，扫描器会把这个冗余对象当成"正在被引用"而留下，
/// 于是每一次「秒传命中 + 崩在墓碑之前」都在桶里多一个永远没人引用的孤儿；
/// 而且现有那 42 条用例一条都不会因此变红。
#[tokio::test]
async fn sweep_s3_deletes_the_redundant_object_when_the_row_points_elsewhere() {
    const UPLOADER: u64 = 9_974_115;
    const FILE_ID: u64 = 9_974_115_01;
    let rig = make_rig().await;
    cleanup(&rig.pool, UPLOADER).await.expect("用例前清库");
    run_with_cleanup(&rig.pool, UPLOADER, async {
        let (_, token) = create_s3_session(&rig, UPLOADER, FILE_ID).await;
        let dir = expire_session(&rig, &token);
        rig.backend.set_list_err(NumberedPartError::NoSuchUpload);
        // 行是本人的、身份也全对，但物理坐标是**另一条路径**（秒传复用了先传者的对象）。
        seed_published_object(
            &rig.pool,
            FILE_ID as i64,
            UPLOADER as i64,
            "files/somebody-elses-object.bin",
            1,
            &plaintext_sha256_of(FILE_ID),
            &sealed_of(FILE_ID),
        )
        .await;
        // final_key 上的对象归属本会话（是这次 MPU 刚发布的那份冗余对象）。
        rig.probe.set_head(Some(upload_id_of(&token)), total_size());

        assert_eq!(sweep_s3(&rig).await, 1);
        assert_eq!(
            rig.probe.delete_calls(),
            1,
            "🔴 final_key 上这份没人引用，必须条件删除，不能当成'正在被引用'留下"
        );
        assert!(!dir.exists(), "对象处置完毕，目录可删");
        // PG 行本身完全正当，不该被牵连。
        assert_eq!(file_row_count(&rig, FILE_ID).await, 1, "引用行必须保留");
    })
    .await;
}
