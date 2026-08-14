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
const UPLOADER_W1: u64 = 9_970_105;
const UPLOADER_W2: u64 = 9_970_106;
const UPLOADER_W3: u64 = 9_970_107;

const BOUNDARY: &str = "----privchatE2EBoundary";

// ---------------------------------------------------------------- 装配

struct Rig {
    state: FileServerState,
    root: PathBuf,
    // 持有 tempdir 的所有权：drop 掉就删目录。
    _dir: tempfile::TempDir,
    // 跨盘跑法下，`tmp/` 指向的那个**另一个盘上**的目录。它不在 tempdir 里，
    // 没人管就会一直堆积。
    _xdev: Option<XdevDir>,
}

/// 另一个文件系统上的临时目录：**谁建的谁删**。
///
/// 🔴 它落在 `TempDir` 管辖之外——`tmp/` 只是一个指过去的符号链接，删掉链接不会动到
/// 目标。少了这个 guard，每跑一遍全量测试就在那个盘上留下一堆目录；RAM 盘只有几十兆，
/// 攒够了之后测试会因为「设备没空间」而失败，而那个失败和被测代码毫无关系——
/// 排查它的人要先趟完一遍完全无关的路。
struct XdevDir(PathBuf);

impl Drop for XdevDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn pool() -> Option<Arc<sqlx::PgPool>> {
    let url = privchat::require_test_database_url()?;
    Some(Arc::new(
        PgPoolOptions::new()
            .max_connections(if std::env::var_os("PRIVCHAT_E2E_TOKEN").is_some() {
                // 崩溃子进程只跑一个请求，占一条就够；抢多了会在全量套件里把
                // Postgres 的连接数耗光，表现成一个跟上传毫无关系的超时。
                1
            } else {
                4
            })
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
    rig_at(root, pool, Some(dir)).await
}

/// 在**指定**根目录上装配。崩溃用例的子进程要用父进程那份存储根和同一把签名密钥，
/// 否则两边看到的根本不是同一个会话。
async fn rig_at(root: PathBuf, pool: Arc<sqlx::PgPool>, keep: Option<tempfile::TempDir>) -> Rig {
    let dir = keep.unwrap_or_else(|| tempfile::tempdir().expect("tempdir"));
    let xdev = link_temp_to_other_filesystem(&root);
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
        _xdev: xdev,
    }
}

/// 设了 `PRIVCHAT_TEST_XDEV_DIR` 时，把存储根下的 `tmp/` 指到**另一个文件系统**上。
///
/// 🔴 这不是给单元测试用的花招，而是真实部署形态：上传盘单独挂出来之后，会话临时目录
/// 与正式对象就不同盘了，发布必须走 `EXDEV` 降级。整套 E2E 换个环境变量再跑一遍，
/// 覆盖的就是**跨挂载点的完整 HTTP 上传**，而不只是发布函数那一段。
///
/// 没设就是同盘（今天生产的形态），照常跑。
///
/// 返回的 guard 由**创建者**持有。子进程看到链接已经在了会直接返回 `None`：
/// 共享目录不能让一个随时会被 SIGKILL 的进程负责清理。
fn link_temp_to_other_filesystem(root: &Path) -> Option<XdevDir> {
    let xdev = std::env::var_os("PRIVCHAT_TEST_XDEV_DIR").map(PathBuf::from)?;
    let link = root.join("tmp");
    if link.exists() {
        return None;
    }
    // 🔴 **独占创建**，不是「pid + 纳秒大概不会重」。
    //
    // 两个并行用例撞上同一个名字的话，两边会共用同一个目录，而先 drop 的那个 guard
    // 会把另一个**还在用**的目录删掉——表现出来是一个跟上传毫无关系的诡异失败。
    // 生产的中转文件已经按这条规矩改成 `create_new` 了，测试 fixture 没有理由更松。
    let far = tempfile::Builder::new()
        .prefix("pcx-e2e-")
        .tempdir_in(&xdev)
        .expect("另一个盘上的临时目录")
        // 目录的生命周期交给 `XdevDir` 管，这里只取路径。
        .keep();
    std::fs::create_dir_all(root).expect("存储根");
    std::os::unix::fs::symlink(&far, &link).expect("把 tmp/ 链到另一个盘");
    Some(XdevDir(far))
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

/// 子进程入口：**跑一次真正的上传请求**，然后死在指定的窗口里。
///
/// 🔴 必须是另一个进程，而且必须走真实路由。同进程里 `ModeGuard::drop` 一定会跑、
/// 异步任务会被规规矩矩地取消——那正好把要测的场景（进程当场消失、状态没来得及
/// 回滚、锁由内核释放）掩盖掉。手工调 `begin_whole()` 再写文件也不行：那验的是
/// 我对流程的复述，不是流程本身。
///
/// 两种死法，对应两类真实事故：
///   · `PRIVCHAT_E2E_STALL=1`：请求体发一半就不发了，由父进程 SIGKILL——
///     模拟传输途中进程被杀，服务端 writer 正开着；
///   · `PRIVCHAT_CRASH_POINT=<窗口名>`：服务端跑到那个窗口自己 `abort()`——
///     模拟在提交序列的某个缝里掉电。
///
/// `#[ignore]` 保证它只在父进程显式点名时执行。
#[tokio::test]
#[ignore]
async fn crash_child_runs_a_real_upload() {
    let root = PathBuf::from(std::env::var("PRIVCHAT_E2E_ROOT").expect("root"));
    let uid: u64 = std::env::var("PRIVCHAT_E2E_UID").unwrap().parse().unwrap();
    let seed: u8 = std::env::var("PRIVCHAT_E2E_SEED").unwrap().parse().unwrap();
    let len: usize = std::env::var("PRIVCHAT_E2E_LEN").unwrap().parse().unwrap();
    let token = std::env::var("PRIVCHAT_E2E_TOKEN").unwrap();
    let stall = std::env::var("PRIVCHAT_E2E_STALL").is_ok();

    let pool = pool().await.expect("child needs a database");
    let rig = rig_at(root, pool, None).await;
    let body = payload(len, seed);
    let _ = uid;

    if stall {
        // 请求体只发前半截，剩下的永远不来。服务端会一直开着 writer 等——
        // 父进程正是在这个状态下开枪。
        let whole = multipart_body(&body);
        let head = bytes::Bytes::copy_from_slice(&whole[..whole.len() / 2]);
        eprintln!("[child] 准备发 {} 字节（共 {}）", head.len(), whole.len());
        // 🔴 小块之间要**隔开时间**，不能一次性把它们塞进流里。
        //
        // multer 会把连续可得的数据合并成一个 chunk 交给 handler，于是服务端只发生
        // **一次** `write`；而 opendal 的 writer 手里压着最近一次 write 的缓冲不下盘，
        // 一次 write 的结果就是磁盘上一个字节都没有。隔开时间才会有多次 write，
        // 才谈得上「传输途中已有部分字节落盘」。
        let chunks: Vec<bytes::Bytes> = head
            .chunks(8 * 1024)
            .map(bytes::Bytes::copy_from_slice)
            .collect();
        let stream = futures::StreamExt::chain(
            futures::stream::unfold(chunks.into_iter(), |mut it| async move {
                let next = it.next()?;
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                Some((Ok::<_, std::io::Error>(next), it))
            }),
            futures::stream::pending::<Result<bytes::Bytes, std::io::Error>>(),
        );
        let router = privchat::http::routes::upload::create_route().with_state(rig.state.clone());
        let _ = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/app/files/upload")
                    .header("X-Upload-Token", &token)
                    .header(
                        "content-type",
                        format!("multipart/form-data; boundary={BOUNDARY}"),
                    )
                    .body(Body::from_stream(stream))
                    .expect("request"),
            )
            .await;
        unreachable!("stall 模式不该拿到响应");
    }

    // 崩溃点模式：`PRIVCHAT_CRASH_POINT` 由父进程通过环境传进来，服务端代码
    // 跑到那个窗口就 abort()。所以这个 await 正常情况下**不会**返回。
    let (status, json) = rig.post(&token, &body).await;
    // 🔴 这里**不能** panic：panic 也是非零退出，父进程就分不清「死在崩溃点」
    // 和「崩溃点根本没生效」了——后者会让整组用例变成什么都没验的假绿。
    eprintln!("崩溃点没有命中，请求正常返回了：{status} {json}");
    std::process::exit(NO_CRASH_EXIT_CODE);
}

/// 子进程用来说「我跑完了，崩溃点没生效」的退出码。
const NO_CRASH_EXIT_CODE: i32 = 97;

/// 子进程句柄：**drop 时一定把它杀掉**。
///
/// 🔴 少了这个，任何一次断言失败都会留下一个孤儿子进程；它继承着 cargo 的 stdout
/// 管道，于是 cargo 会一直等 EOF——测试不是失败，而是**挂住**。第一版就是这么挂的：
/// 我在排查一个假死，真正的错误信息还在被那根管道憋着。
struct ChildGuard(std::process::Child);

impl ChildGuard {
    fn id(&self) -> u32 {
        self.0.id()
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        self.0.wait().expect("wait child")
    }
    /// 已经退出就给出状态，还活着就是 `None`。
    fn try_wait(&mut self) -> Option<std::process::ExitStatus> {
        self.0.try_wait().expect("try_wait child")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// 等一个条件成立，**同时盯着子进程别死了**。
///
/// 🔴 只等条件的话，子进程一旦提前失败（连不上库、装配报错），这里会一声不吭地干等
/// 到超时，最后报的是「等 XXX 超时」——真正的原因在子进程的输出里，而排查的人手上
/// 只有一句和病因无关的超时。子进程先退出就立刻带着它的退出状态失败。
fn wait_until_child_alive(what: &str, child: &mut ChildGuard, mut ready: impl FnMut() -> bool) {
    // 预算给足：全量套件并行跑的时候，子进程连库、装配都可能被拖慢，
    // 而这个等待本身不是被测对象。
    for _ in 0..1200 {
        if ready() {
            return;
        }
        if let Some(status) = child.try_wait() {
            panic!("等 {what} 的时候子进程先退出了：{status:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("等待 {what} 超时（60s）");
}

/// 起一个子进程跑真实上传。`crash_point` 为 `None` 时是 stall 模式。
fn spawn_crash_child(
    root: &Path,
    uid: u64,
    token: &str,
    len: usize,
    seed: u8,
    crash_point: Option<&str>,
) -> ChildGuard {
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("test exe"));
    cmd.args(["--exact", "crash_child_runs_a_real_upload", "--ignored", "--nocapture"])
        .env("PRIVCHAT_E2E_ROOT", root)
        .env("PRIVCHAT_E2E_UID", uid.to_string())
        .env("PRIVCHAT_E2E_TOKEN", token)
        .env("PRIVCHAT_E2E_LEN", len.to_string())
        .env("PRIVCHAT_E2E_SEED", seed.to_string());
    match crash_point {
        Some(p) => {
            cmd.env("PRIVCHAT_CRASH_POINT", p);
        }
        None => {
            cmd.env("PRIVCHAT_E2E_STALL", "1");
        }
    }
    ChildGuard(cmd.spawn().expect("spawn child"))
}

/// 子进程应当**死于信号**（`abort()` 发的 `SIGABRT`）。
///
/// 🔴 判据只能是「被信号杀死」，不能是「退出码非零」。子进程里任何一个 panic 也是
/// 非零退出，用非零当判据的话，「崩溃注入根本没生效」会被记成通过——那一整组恢复
/// 用例就变成了什么都没验的假绿。
fn expect_child_aborted(child: &mut ChildGuard, window: &str) {
    use std::os::unix::process::ExitStatusExt;
    let status = child.wait();
    if status.code() == Some(NO_CRASH_EXIT_CODE) {
        panic!("崩溃点 {window} 没生效：子进程把整个上传正常跑完了");
    }
    assert_eq!(
        status.signal(),
        Some(libc::SIGABRT),
        "子进程本该在窗口 {window} 被 abort() 杀死，实际 {status:?}",
    );
}

/// 库里有几行属于这个 uploader。
async fn row_count(pool: &sqlx::PgPool, uploader: u64) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1")
        .bind(uploader as i64)
        .fetch_one(pool)
        .await
        .expect("count rows")
}

/// 会话里预留的 `file_id`（崩溃后从磁盘读回来）。
fn reserved_id(rig: &Rig, uid: u64, upload_id: &str) -> Option<u64> {
    UploadSession::open_existing(&rig.session_root(), uid, upload_id)
        .expect("open session")
        .expect("session exists")
        .reserved_file_id()
        .expect("read reservation")
}

/// 已经发布到正式区的对象（`tmp/` 之外的一切文件）。
///
/// 用它而不是从库里查 `file_path`：崩溃发生在落库之前时，库里**本来就没有行**，
/// 而「对象是不是已经躺在正式路径上」恰恰是那一刻唯一要问的事。
fn published_objects(root: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().is_some_and(|n| n == "tmp") {
                    continue;
                }
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, &mut out);
    out
}

/// 传输途中进程被 SIGKILL：服务端 writer 正开着，请求体只到了一半。
///
/// 这是**真的一次 HTTP 请求**被打断——不是手工摆出来的会话状态。
#[tokio::test]
async fn an_upload_survives_a_kill_mid_transfer() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_CRASH]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(512 * 1024 + 13, 7);
    let (token, upload_id) = rig.issue(UPLOADER_CRASH, &body).await;

    let staging = rig.staging(UPLOADER_CRASH, &upload_id);
    let mut child = spawn_crash_child(&rig.root, UPLOADER_CRASH, &token, body.len(), 7, None);

    // 等到服务端真的在往 body.part 上写字节，再开枪。
    // 等到服务端**已经开着 writer**。判据是 body.part 出现，而不是它长到多少字节：
    // 存储层什么时候把缓冲刷下去是它自己的事，拿字节数当同步点会把测试绑死在
    // opendal 的缓冲策略上。文件一出现就说明 token 验过了、会话占住了、预留落盘了、
    // writer 开着——正是要在这一刻开枪的状态。
    // 判据是**磁盘上真的有字节**，不是「文件出现了」。writer 一打开文件就存在，
    // 拿存在当同步点会在零字节时就开枪，那测的是「还没开始写」的恢复，不是
    // 「写了一半」的恢复——恰好把要覆盖的场景漏掉。
    wait_until_child_alive("子进程把部分字节写到 body.part", &mut child, || {
        std::fs::metadata(&staging).map(|m| m.len() > 0).unwrap_or(false)
    });

    // SIGKILL：没有清理、没有 Drop、没有回滚。
    // SAFETY: pid 来自我们刚 spawn 的子进程。
    unsafe { libc::kill(child.id() as i32, libc::SIGKILL) };
    assert!(!child.wait().success());

    // 崩溃现场：状态停在接收中，半截字节还在，什么都没落库。
    assert_eq!(
        rig.session_status(UPLOADER_CRASH, &upload_id),
        UploadStatus::WholeReceiving,
        "SIGKILL 之后状态就该停在 WholeReceiving —— 这正是恢复逻辑要面对的现实"
    );
    let partial = std::fs::metadata(&staging).expect("残留的临时对象").len();
    assert!(
        partial > 0 && partial < body.len() as u64,
        "崩溃现场必须是**写了一半**：0 说明还没开始写，等于完整长度说明根本没被打断。实际 {partial}"
    );
    assert_eq!(row_count(&pool, UPLOADER_CRASH).await, 0);
    // 🔴 预留值必须**在重试之前**读出来单独存着。
    //
    // 早先这里写的是 `reserved_id(...).map(|_| file_id)`：它把预留值换成了返回值，
    // 于是只要「预留存在」就恒等，根本没比较过两者——一条永远为真的断言。
    let reserved_before_retry = reserved_id(&rig, UPLOADER_CRASH, &upload_id)
        .expect("接收 body 之前就该把预留写进会话");

    // 重试：同一张 token 从头传完整的一份。
    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "崩溃后必须还能传完：{json}");
    let file_id = file_id_of(&json);
    assert_eq!(
        file_id, reserved_before_retry,
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

/// 窗口一：字节收完、校验通过，**还没发布**就掉电。
///
/// 正式区必须干干净净——这一刻还没有任何东西该出现在那儿。
#[tokio::test]
async fn a_crash_before_publish_leaves_the_final_area_empty() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_W1]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(20_000, 21);
    let (token, upload_id) = rig.issue(UPLOADER_W1, &body).await;

    let mut child = spawn_crash_child(
        &rig.root,
        UPLOADER_W1,
        &token,
        body.len(),
        21,
        Some("after_verify_before_publish"),
    );
    expect_child_aborted(&mut child, "after_verify_before_publish");

    assert!(
        published_objects(&rig.root).is_empty(),
        "发布之前崩溃，正式区不该有任何对象：{:?}",
        published_objects(&rig.root)
    );
    assert_eq!(row_count(&pool, UPLOADER_W1).await, 0);
    let reserved = reserved_id(&rig, UPLOADER_W1, &upload_id).expect("预留必须已经落盘");

    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(file_id_of(&json), reserved, "重试必须复用预留的 file_id");
    assert_eq!(row_count(&pool, UPLOADER_W1).await, 1);
    let on_disk = std::fs::read(stored_path(&rig, reserved).await).expect("读正式对象");
    assert_eq!(on_disk, body);

    cleanup(&pool, &[UPLOADER_W1]).await;
}

/// 窗口二：对象已经发布，事务**还没提交**就掉电。
///
/// 🔴 这是最危险的一格：正式路径上有对象、库里没有行。重试必须核验后**接着落库**，
/// 既不能报「已存在」失败，更不能覆盖那个对象。
#[tokio::test]
async fn a_crash_after_publish_recovers_without_overwriting() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_W2]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(20_000, 33);
    let (token, upload_id) = rig.issue(UPLOADER_W2, &body).await;

    let mut child = spawn_crash_child(
        &rig.root,
        UPLOADER_W2,
        &token,
        body.len(),
        33,
        Some("after_publish_before_commit"),
    );
    expect_child_aborted(&mut child, "after_publish_before_commit");

    // 崩溃现场：对象在，行不在。
    let published = published_objects(&rig.root);
    assert_eq!(published.len(), 1, "正式区应当正好有那一个已发布对象：{published:?}");
    assert_eq!(
        std::fs::read(&published[0]).expect("读已发布对象"),
        body,
        "已发布的对象内容必须是完整的那一份"
    );
    assert_eq!(row_count(&pool, UPLOADER_W2).await, 0, "事务没提交，就不该有行");
    assert!(
        !rig.staging(UPLOADER_W2, &upload_id).exists(),
        "发布之后临时对象就该消失"
    );
    let inode_before = file_identity(&published[0]);

    // 重试：核验一致 → 直接继续落库。
    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "已发布未提交必须能恢复：{json}");
    let file_id = file_id_of(&json);
    assert_eq!(row_count(&pool, UPLOADER_W2).await, 1);
    assert_eq!(
        stored_path(&rig, file_id).await,
        published[0],
        "落的库必须指向崩溃前那个对象"
    );
    assert_eq!(
        file_identity(&published[0]),
        inode_before,
        "🔴 已发布的对象被重新写了一遍——恢复路径必须核验后跳过，绝不能覆盖"
    );

    cleanup(&pool, &[UPLOADER_W2]).await;
}

/// 窗口三：记录已经提交，**墓碑还没写**就掉电。
///
/// 客户端重发时必须拿回同一个 `file_id`，而且不能多出第二行。
#[tokio::test]
async fn a_crash_before_the_tombstone_still_answers_the_retry() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_W3]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(20_000, 44);
    let (token, upload_id) = rig.issue(UPLOADER_W3, &body).await;

    let mut child = spawn_crash_child(
        &rig.root,
        UPLOADER_W3,
        &token,
        body.len(),
        44,
        Some("after_commit_before_tombstone"),
    );
    expect_child_aborted(&mut child, "after_commit_before_tombstone");

    // 崩溃现场：行已经在了，墓碑没写上。
    assert_eq!(row_count(&pool, UPLOADER_W3).await, 1, "事务提交过了");
    assert_ne!(
        rig.session_status(UPLOADER_W3, &upload_id),
        UploadStatus::Completed,
        "墓碑还没写——这正是这一格要制造的现场"
    );
    let reserved = reserved_id(&rig, UPLOADER_W3, &upload_id).expect("预留");

    // 重试：靠 `reserved_file_id` 撞主键 → 回读那一行。
    let (status, json) = rig.post(&token, &body).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    assert_eq!(file_id_of(&json), reserved, "必须回到崩溃前那一行");
    assert_eq!(row_count(&pool, UPLOADER_W3).await, 1, "不能多出第二行");

    cleanup(&pool, &[UPLOADER_W3]).await;
}

/// 文件的物理身份（设备 + inode + 修改时间）。
///
/// 用它来回答「这个对象有没有被重新写过」——只比内容是答不了的：
/// 覆盖成同样的字节，内容比对照样通过。
fn file_identity(path: &Path) -> (u64, u64, std::time::SystemTime) {
    use std::os::unix::fs::MetadataExt;
    let m = std::fs::metadata(path).expect("stat");
    (m.dev(), m.ino(), m.modified().expect("mtime"))
}
