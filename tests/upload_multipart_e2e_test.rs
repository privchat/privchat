// 整包上传的**端到端**门禁：真的 multipart 请求打进真的 axum 路由，字节落到真的
// 磁盘，记录进真的 Postgres。
//
// 🔴 为什么必须是端到端的：整包路径的正确性分散在四个地方——token 验证、会话状态机、
// 临时对象发布、落库收敛。单元测试各自都能绿，而线上真正会坏的是它们的**接缝**：
// 「预留 id 落盘了吗」「崩溃后状态卡住了吗」「临时对象清干净了吗」「重试拿到的是同一个
// file_id 还是第二条记录」。这些问题只有让一个完整请求走完全程才会暴露。
//
// 崩溃用例用的是**真的另一个进程 + 真的 SIGKILL**：同进程里 `Drop` 一定会执行，
// 于是「持锁进程猝死、状态没来得及回滚」这个恰恰最危险的场景根本构造不出来——
// 上一轮的会话单测就是这么漏掉「崩溃后卡在 WholeReceiving 永久锁死」的。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::model::file_upload::FileType;
use privchat::security::upload_token::{IssueMode, UploadTokenConfig};
use privchat::service::file_service::FileService;
use privchat::service::upload_session::{UploadSession, UploadStatus};
use privchat::service::upload_token_service::{
    UploadIdentity, UploadTokenPurpose, UploadTokenService,
};

/// 各用例独占一个 uploader：真库是共享的，并行跑的用例之间只要共用 uploader_id，
/// 一方的清理就会踩掉另一方的前提，失败点离病因很远。
const UPLOADER_HAPPY: u64 = 9_970_101;
const UPLOADER_IDEMPOTENT: u64 = 9_970_102;
const UPLOADER_MISMATCH: u64 = 9_970_103;
const UPLOADER_CRASH: u64 = 9_970_104;

const BOUNDARY: &str = "----privchatE2EBoundary";

// ---------------------------------------------------------------- 装配

struct Rig {
    state: FileServerState,
    root: PathBuf,
    // 持有 tempdir 的所有权：drop 掉就删目录。
    _dir: tempfile::TempDir,
}

async fn pool() -> Option<Arc<sqlx::PgPool>> {
    let url = privchat::require_test_database_url()?;
    Some(Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    ))
}

fn signing_config() -> UploadTokenConfig {
    UploadTokenConfig {
        keys: [(
            "e2e".to_string(),
            "e2e-upload-secret-e2e-upload-secret".to_string(),
        )]
        .into_iter()
        .collect(),
        default_kid: "e2e".to_string(),
        leeway_secs: 30,
        ttl_secs: 24 * 3600,
        issue_mode: IssueMode::Signed,
    }
}

async fn rig(pool: Arc<sqlx::PgPool>) -> Rig {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let source = FileStorageSourceConfig {
        id: 0,
        storage_type: "local".to_string(),
        storage_root: root.to_string_lossy().to_string(),
        base_url: Some("http://e2e.local/files".to_string()),
        endpoint: None,
        bucket: None,
        access_key_id: None,
        secret_access_key: None,
        path_prefix: None,
    };
    let file_service = FileService::new(vec![source], 0, pool);
    file_service.init().await.expect("init storage");

    Rig {
        state: FileServerState {
            file_service: Arc::new(file_service),
            upload_token_service: Arc::new(
                UploadTokenService::new().with_signing(Some(signing_config())),
            ),
        },
        root,
        _dir: dir,
    }
}

impl Rig {
    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    /// 签一张整包上传 token，返回 (token, upload_id)。
    async fn issue(&self, uid: u64, body: &[u8]) -> (String, String) {
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body));
        let (token, upload_id, _exp) = self
            .state
            .upload_token_service
            .issue(
                Self::now(),
                uid,
                FileType::File,
                10 * 1024 * 1024,
                "message".to_string(),
                Some("e2e.bin".to_string()),
                UploadIdentity {
                    sha256: Some(sha),
                    declared_size: Some(body.len() as i64),
                    mime_type: Some("application/octet-stream".to_string()),
                    transform_version: 0,
                },
                UploadTokenPurpose::Upload,
                None,
            )
            .await
            .expect("issue token");
        (token, upload_id)
    }

    async fn post(&self, token: &str, body: &[u8]) -> (StatusCode, serde_json::Value) {
        let router = privchat::http::routes::upload::create_route().with_state(self.state.clone());
        let response = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/app/files/upload")
                    .header("X-Upload-Token", token)
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::from(multipart_body(body)))
                    .expect("request"),
            )
            .await
            .expect("router response");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .expect("read body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    fn session_root(&self) -> PathBuf {
        self.root.join("tmp/uploads")
    }

    fn staging(&self, uid: u64, upload_id: &str) -> PathBuf {
        self.session_root()
            .join(uid.to_string())
            .join(upload_id)
            .join("body.part")
    }

    fn session_status(&self, uid: u64, upload_id: &str) -> UploadStatus {
        UploadSession::open_existing(&self.session_root(), uid, upload_id)
            .expect("open session")
            .expect("session exists")
            .read_state()
            .expect("read state")
            .status
    }
}

/// 手搓 multipart：这里刻意**不用**客户端库。上传协议的对端是三个不同技术栈的
/// 客户端，测试要盯的是线上真正传过来的那串字节长什么样。
fn multipart_body(file: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    out.extend_from_slice(
        b"Content-Disposition: form-data; name=\"file\"; filename=\"e2e.bin\"\r\n",
    );
    out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    out.extend_from_slice(file);
    out.extend_from_slice(format!("\r\n--{BOUNDARY}\r\n").as_bytes());
    out.extend_from_slice(b"Content-Disposition: form-data; name=\"encryption_version\"\r\n\r\n0");
    out.extend_from_slice(format!("\r\n--{BOUNDARY}--\r\n").as_bytes());
    out
}

/// 可复现的伪随机负载：内容必须逐字节可断言，不能用全 0（全 0 的截断和覆盖看起来一样）。
fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

async fn cleanup(pool: &sqlx::PgPool, uploaders: &[u64]) {
    let ids: Vec<i64> = uploaders.iter().map(|u| *u as i64).collect();
    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = ANY($1)")
        .bind(&ids)
        .execute(pool)
        .await
        .expect("clean uploads");
}

fn file_id_of(json: &serde_json::Value) -> u64 {
    assert_eq!(json["code"], 0, "响应不是成功：{json}");
    json["data"]["file_id"]
        .as_u64()
        .or_else(|| {
            json["data"]["file_id"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or_else(|| panic!("响应里没有 file_id：{json}"))
}

/// 正式对象的绝对路径。响应里的 `file_url` 是访问地址，磁盘位置要从库里的
/// `file_path` 取——两者不是一回事，混用会让断言看着过了其实什么都没验。
async fn stored_path(rig: &Rig, file_id: u64) -> PathBuf {
    let meta = rig
        .state
        .file_service
        .get_file_metadata(file_id)
        .await
        .expect("query metadata")
        .expect("metadata row");
    rig.root.join(meta.file_path.trim_start_matches('/'))
}

// ---------------------------------------------------------------- 用例

/// 一次成功的整包上传，走完全程。
#[tokio::test]
async fn a_whole_upload_lands_on_disk_and_in_the_database() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_HAPPY]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(64 * 1024 + 7, 3);
    let (token, upload_id) = rig.issue(UPLOADER_HAPPY, &body).await;
    let (status, json) = rig.post(&token, &body).await;

    assert_eq!(status, StatusCode::OK, "{json}");
    let file_id = file_id_of(&json);

    // 字节确实在正式路径上，而且一个不差。
    let path = stored_path(&rig, file_id).await;
    let on_disk = std::fs::read(&path).expect("读正式对象");
    assert_eq!(on_disk, body, "落盘内容与上传内容不一致");

    // 临时对象不留：客户端可能上传成功后立刻离线，清理不能等回调。
    assert!(
        !rig.staging(UPLOADER_HAPPY, &upload_id).exists(),
        "发布之后 body.part 应当已经删掉"
    );

    // 墓碑立起来了，迟到的重复请求靠它拿到同一个结果。
    assert_eq!(
        rig.session_status(UPLOADER_HAPPY, &upload_id),
        UploadStatus::Completed
    );

    cleanup(&pool, &[UPLOADER_HAPPY]).await;
}

/// 响应丢了、客户端重发：必须拿回**同一个** file_id，且不产生第二条记录。
#[tokio::test]
async fn a_repeated_post_returns_the_same_file_id() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_IDEMPOTENT]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(4096, 11);
    let (token, _upload_id) = rig.issue(UPLOADER_IDEMPOTENT, &body).await;

    let (s1, j1) = rig.post(&token, &body).await;
    assert_eq!(s1, StatusCode::OK, "{j1}");
    let (s2, j2) = rig.post(&token, &body).await;
    assert_eq!(s2, StatusCode::OK, "{j2}");

    assert_eq!(
        file_id_of(&j1),
        file_id_of(&j2),
        "重复 POST 必须回到同一个 file_id"
    );

    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1")
            .bind(UPLOADER_IDEMPOTENT as i64)
            .fetch_one(pool.as_ref())
            .await
            .expect("count rows");
    assert_eq!(rows, 1, "重复 POST 不能写出第二条记录");

    cleanup(&pool, &[UPLOADER_IDEMPOTENT]).await;
}

/// 字节与 prepare 声明的不符：拒绝，正式路径上不能留下任何东西，
/// 而且**同一张 token 还能重试**（失败不该把上传永久锁死）。
#[tokio::test]
async fn a_tampered_body_is_rejected_and_leaves_nothing_behind() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_MISMATCH]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(8192, 5);
    let (token, upload_id) = rig.issue(UPLOADER_MISMATCH, &body).await;

    // 同样长度、不同内容：只比大小的实现会放它过去。
    let tampered = payload(8192, 6);
    let (status, json) = rig.post(&token, &tampered).await;
    assert_ne!(status, StatusCode::OK, "篡改的字节不该被接受：{json}");

    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1")
            .bind(UPLOADER_MISMATCH as i64)
            .fetch_one(pool.as_ref())
            .await
            .expect("count rows");
    assert_eq!(rows, 0, "被拒的上传不能落库");
    assert!(
        !rig.staging(UPLOADER_MISMATCH, &upload_id).exists(),
        "被拒之后临时对象要删掉"
    );
    assert_eq!(
        rig.session_status(UPLOADER_MISMATCH, &upload_id),
        UploadStatus::Idle,
        "失败后状态要回到 Idle，否则同一张 token 再也传不了"
    );

    // 同一张 token 用正确字节重试，应当成功。
    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    let file_id = file_id_of(&json);
    let on_disk = std::fs::read(stored_path(&rig, file_id).await).expect("读正式对象");
    assert_eq!(on_disk, body);

    cleanup(&pool, &[UPLOADER_MISMATCH]).await;
}

// ---------------------------------------------------------------- 跨进程崩溃

/// 子进程入口：占住会话、写一段残留字节，然后**等着被 SIGKILL**。
///
/// 🔴 必须是另一个进程。同进程里 `ModeGuard::drop` 一定会跑，状态会被规规矩矩地
/// 回滚成 `Idle`——那正好把要测的场景（状态卡在 `WholeReceiving`、锁由内核释放）
/// 掩盖掉。`#[ignore]` 保证它只在父进程显式点名时执行。
#[test]
#[ignore]
fn sigkill_child_occupies_the_session() {
    let root = PathBuf::from(std::env::var("PRIVCHAT_E2E_SESSION_ROOT").expect("session root"));
    let uid: u64 = std::env::var("PRIVCHAT_E2E_UID").unwrap().parse().unwrap();
    let upload_id = std::env::var("PRIVCHAT_E2E_UPLOAD_ID").unwrap();
    let staging = std::env::var("PRIVCHAT_E2E_STAGING").unwrap();

    let session = UploadSession::open(&root, uid, &upload_id).expect("open session");
    let guard = session.begin_whole().expect("begin whole");

    // 半截字节：崩溃发生在接收途中。
    std::fs::write(&staging, payload(1024, 99)).expect("write partial");

    std::fs::write(session.dir().join("child.ready"), b"1").expect("ready flag");

    // 别让守卫在任何路径上被 drop——这个进程的结局只有 SIGKILL。
    std::mem::forget(guard);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn wait_for(path: &Path, what: &str) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("等待 {what} 超时：{path:?}");
}

/// 上传途中进程被 SIGKILL：磁盘上留下「接收中」的状态和半截字节，
/// 重试必须能接着传完，而不是被这次崩溃永久锁死。
#[tokio::test]
async fn an_upload_survives_a_sigkilled_process() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_CRASH]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(32 * 1024 + 13, 7);
    let (token, upload_id) = rig.issue(UPLOADER_CRASH, &body).await;

    // 预留排在收字节之前——崩溃重试要复用同一个 file_id。
    let reserved = rig
        .state
        .file_service
        .reserve_file_id()
        .await
        .expect("reserve file id");
    {
        let session = UploadSession::open(&rig.session_root(), UPLOADER_CRASH, &upload_id)
            .expect("open session");
        session
            .reserve_file_id(reserved)
            .expect("write reservation");
    }
    let staging = rig.staging(UPLOADER_CRASH, &upload_id);
    std::fs::create_dir_all(staging.parent().unwrap()).expect("staging dir");

    let mut child = std::process::Command::new(std::env::current_exe().expect("test exe"))
        .args(["--exact", "sigkill_child_occupies_the_session", "--ignored"])
        .env("PRIVCHAT_E2E_SESSION_ROOT", rig.session_root())
        .env("PRIVCHAT_E2E_UID", UPLOADER_CRASH.to_string())
        .env("PRIVCHAT_E2E_UPLOAD_ID", &upload_id)
        .env("PRIVCHAT_E2E_STAGING", &staging)
        .spawn()
        .expect("spawn child");

    let session_dir = rig
        .session_root()
        .join(UPLOADER_CRASH.to_string())
        .join(&upload_id);
    wait_for(&session_dir.join("child.ready"), "子进程占住会话");

    // SIGKILL：没有清理、没有 Drop、没有回滚。
    // SAFETY: pid 来自我们刚 spawn 的子进程。
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    let status = child.wait().expect("wait child");
    assert!(!status.success(), "子进程应当死于 SIGKILL");

    // 崩溃现场：状态停在接收中，半截字节还在。
    assert_eq!(
        rig.session_status(UPLOADER_CRASH, &upload_id),
        UploadStatus::WholeReceiving,
        "SIGKILL 之后状态就该停在 WholeReceiving —— 这正是恢复逻辑要面对的现实"
    );
    assert!(staging.exists(), "残留的半截字节应当还在");

    // 重试：同一张 token 从头传完整的一份。
    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "崩溃后必须还能传完：{json}");
    let file_id = file_id_of(&json);
    assert_eq!(
        file_id, reserved,
        "重试必须复用崩溃前预留的 file_id，否则上一次的半成品没人认领"
    );

    // 🔴 残留的半截字节不能被接在新内容前面：writer 必须是覆盖而不是追加。
    let on_disk = std::fs::read(stored_path(&rig, file_id).await).expect("读正式对象");
    assert_eq!(on_disk, body, "崩溃残留的字节污染了这次上传");
    assert!(!staging.exists(), "发布之后临时对象要删掉");
    assert_eq!(
        rig.session_status(UPLOADER_CRASH, &upload_id),
        UploadStatus::Completed
    );

    cleanup(&pool, &[UPLOADER_CRASH]).await;
}
