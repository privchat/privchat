// 断点续传的端到端门禁：分片写入 → 断掉 → 查缺口 → 接着传 → 完成。
//
// 🔴 这一套盯的是**功能本身跑不跑得通**，而不是某个函数的返回值：真的 HTTP 请求打进
// 真的路由，字节落到真的磁盘，记录进真的 Postgres，中间用「换一个进程」来模拟服务端
// 重启——重启之后仍然只补缺口，而不是从头再来，这正是断点续传要给用户的东西。

use std::path::PathBuf;
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
use privchat::service::upload_token_service::{
    UploadIdentity, UploadTokenPurpose, UploadTokenService,
};

const UPLOADER_FLOW: u64 = 9_971_001;
const UPLOADER_RESUME: u64 = 9_971_002;
const UPLOADER_IDEMPOTENT: u64 = 9_971_003;
const UPLOADER_CONFLICT: u64 = 9_971_004;
const UPLOADER_ABORT: u64 = 9_971_005;
const UPLOADER_BIG: u64 = 9_971_006;

/// 服务端下发的寻址网格。分片必须按它对齐。
const BASE_UNIT: usize = 64 * 1024;

struct Rig {
    state: FileServerState,
    root: PathBuf,
    /// 只有「自己开的临时目录」才由自己删。**跨重启的用例必须把目录留在外面**：
    /// 让 Rig 持有它的话，第一段人生结束时连存储根一起删掉，「重启」就成了「换台机器」，
    /// 而那正好把要测的东西——磁盘上的会话还在不在——绕过去了。
    _dir: Option<tempfile::TempDir>,
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
            "resumable-e2e-secret-resumable-e2e".to_string(),
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

/// 在指定根目录上装配。**「服务端重启」就是丢掉这个 Rig 再用同一个根目录建一个新的**：
/// 进程内的一切都没了，能接上就只能靠磁盘上的会话。
async fn rig_at(root: PathBuf, pool: Arc<sqlx::PgPool>, dir: Option<tempfile::TempDir>) -> Rig {
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
    fn router(&self) -> axum::Router {
        privchat::http::routes::upload::create_route().with_state(self.state.clone())
    }

    async fn issue(&self, uid: u64, body: &[u8]) -> String {
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(body));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let (token, _upload_id, _exp) = self
            .state
            .upload_token_service
            .issue(
                now,
                uid,
                FileType::File,
                64 * 1024 * 1024,
                "message".to_string(),
                Some("resumable.bin".to_string()),
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
        token
    }

    async fn send(&self, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let resp = self.router().oneshot(req).await.expect("router response");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("read body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    /// 写一段字节。
    async fn chunk(&self, token: &str, offset: usize, bytes: &[u8]) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/app/files/chunk?offset={offset}"))
                .header("X-Upload-Token", token)
                .body(Body::from(bytes.to_vec()))
                .expect("request"),
        )
        .await
    }

    async fn status(&self, token: &str) -> serde_json::Value {
        let (code, json) = self
            .send(
                Request::builder()
                    .method("GET")
                    .uri("/api/app/files/status")
                    .header("X-Upload-Token", token)
                    .body(Body::empty())
                    .expect("request"),
            )
            .await;
        assert_eq!(code, StatusCode::OK, "{json}");
        json["data"].clone()
    }

    async fn complete(&self, token: &str) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .method("POST")
                .uri("/api/app/files/complete")
                .header("X-Upload-Token", token)
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .expect("request"),
        )
        .await
    }

    async fn abort(&self, token: &str) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .method("POST")
                .uri("/api/app/files/abort")
                .header("X-Upload-Token", token)
                .body(Body::empty())
                .expect("request"),
        )
        .await
    }

    async fn stored_bytes(&self, file_id: u64) -> Vec<u8> {
        let meta = self
            .state
            .file_service
            .get_file_metadata(file_id)
            .await
            .expect("query")
            .expect("row");
        std::fs::read(self.root.join(meta.file_path.trim_start_matches('/'))).expect("读正式对象")
    }
}

/// 可复现且逐字节可断言的负载。
fn payload(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| ((i as u32).wrapping_mul(2654435761) >> 13) as u8 ^ seed)
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
    json["data"]["file_id"].as_u64().expect("file_id")
}

fn ranges(status: &serde_json::Value) -> Vec<(u64, u64)> {
    status["confirmed_ranges"]
        .as_array()
        .expect("ranges")
        .iter()
        .map(|r| (r["offset"].as_u64().unwrap(), r["len"].as_u64().unwrap()))
        .collect()
}

// ---------------------------------------------------------------- 用例

/// 主流程：按 64KiB 网格分片传完 → complete → 字节与库都对。
#[tokio::test]
async fn a_chunked_upload_completes_and_matches_byte_for_byte() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_FLOW]).await;
    let rig = rig(pool.clone()).await;

    // 刻意不是 base_unit 的整数倍：最后一片是短的，这正是最容易写错的地方。
    let body = payload(BASE_UNIT * 3 + 12_345, 0x5a);
    let token = rig.issue(UPLOADER_FLOW, &body).await;

    for (i, part) in body.chunks(BASE_UNIT).enumerate() {
        let (code, json) = rig.chunk(&token, i * BASE_UNIT, part).await;
        assert_eq!(code, StatusCode::OK, "第 {i} 片失败：{json}");
        assert_eq!(json["data"]["outcome"], "confirmed");
    }

    let st = rig.status(&token).await;
    assert_eq!(st["complete"], true, "{st}");
    assert_eq!(st["confirmed_bytes"].as_u64().unwrap(), body.len() as u64);
    assert_eq!(ranges(&st), vec![(0, body.len() as u64)], "应当合并成一段");

    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    let file_id = file_id_of(&json);
    assert_eq!(rig.stored_bytes(file_id).await, body, "落盘内容必须逐字节相同");
    assert_eq!(json["data"]["file_size"].as_u64().unwrap(), body.len() as u64);

    cleanup(&pool, &[UPLOADER_FLOW]).await;
}

/// 🔴 **断点续传本体**：传一半 → 服务端「重启」→ 查缺口 → 只补缺口 → 完成。
///
/// 重启用的是「丢掉整个 Rig，再用同一个存储根建一个新的」：进程内的状态一个都不留，
/// 能接上就只能靠磁盘上的会话。补传时**只发缺的那几片**——多传的话这条用例发现不了，
/// 所以最后要拿「补传字节数」跟「缺口大小」对齐。
#[tokio::test]
async fn an_interrupted_upload_resumes_from_the_gap_after_a_restart() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_RESUME]).await;

    // 🔴 目录留在用例作用域里，跨越两段人生。
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let body = payload(BASE_UNIT * 5, 0x27);
    let parts: Vec<&[u8]> = body.chunks(BASE_UNIT).collect();

    // ---- 第一段人生：传前两片就「断网」了 ----
    let token = {
        let rig = rig_at(root.clone(), pool.clone(), None).await;
        let token = rig.issue(UPLOADER_RESUME, &body).await;
        for i in 0..2 {
            let (code, _) = rig.chunk(&token, i * BASE_UNIT, parts[i]).await;
            assert_eq!(code, StatusCode::OK);
        }
        token
    }; // rig 在这里被丢掉 = 服务端进程没了

    // ---- 第二段人生：新进程，同一个存储根 ----
    let rig = rig_at(root.clone(), pool.clone(), None).await;

    let st = rig.status(&token).await;
    assert_eq!(
        ranges(&st),
        vec![(0, (BASE_UNIT * 2) as u64)],
        "重启后必须还认得已传的那两片：{st}"
    );
    assert!(!st["complete"].as_bool().unwrap());

    // 只补缺口。
    let already = st["confirmed_bytes"].as_u64().unwrap() as usize;
    let mut resent = 0usize;
    for (i, part) in parts.iter().enumerate() {
        let offset = i * BASE_UNIT;
        if offset < already {
            continue; // 已确认，不重传——这就是断点续传省下来的
        }
        let (code, _) = rig.chunk(&token, offset, part).await;
        assert_eq!(code, StatusCode::OK);
        resent += part.len();
    }
    assert_eq!(
        resent,
        body.len() - already,
        "🔴 补传的字节数必须正好等于缺口——多一个字节就说明没在续传"
    );

    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    let file_id = file_id_of(&json);
    assert_eq!(
        rig.stored_bytes(file_id).await,
        body,
        "跨进程拼起来的文件必须与原文逐字节相同"
    );

    cleanup(&pool, &[UPLOADER_RESUME]).await;
}

/// 同一片重传：幂等，不重复计数。
#[tokio::test]
async fn resending_the_same_chunk_is_idempotent() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_IDEMPOTENT]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(BASE_UNIT * 2, 0x11);
    let token = rig.issue(UPLOADER_IDEMPOTENT, &body).await;
    let parts: Vec<&[u8]> = body.chunks(BASE_UNIT).collect();

    let (_, first) = rig.chunk(&token, 0, parts[0]).await;
    assert_eq!(first["data"]["outcome"], "confirmed");
    let (code, again) = rig.chunk(&token, 0, parts[0]).await;
    assert_eq!(code, StatusCode::OK, "{again}");
    assert_eq!(
        again["data"]["outcome"], "already_covered",
        "同一片同样内容重传就该是成功，不是错误"
    );
    assert_eq!(
        again["data"]["confirmed_bytes"].as_u64().unwrap(),
        BASE_UNIT as u64,
        "重传不能把已确认字节数算两遍"
    );

    rig.chunk(&token, BASE_UNIT, parts[1]).await;
    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(rig.stored_bytes(file_id_of(&json)).await, body);

    cleanup(&pool, &[UPLOADER_IDEMPOTENT]).await;
}

/// 🔴 同一区间**不同内容**：拒绝，且磁盘上已确认的字节不能被改动。
#[tokio::test]
async fn a_conflicting_chunk_is_refused_and_changes_nothing() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_CONFLICT]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(BASE_UNIT * 2, 0x33);
    let token = rig.issue(UPLOADER_CONFLICT, &body).await;
    let parts: Vec<&[u8]> = body.chunks(BASE_UNIT).collect();
    rig.chunk(&token, 0, parts[0]).await;

    // 同一区间，换一份内容。
    let evil = payload(BASE_UNIT, 0x99);
    let (code, json) = rig.chunk(&token, 0, &evil).await;
    assert_ne!(code, StatusCode::OK, "冲突的分片不该被接受：{json}");

    // 补完剩下的，完成后内容必须还是原文——被拒的那片一个字节都没写进去。
    rig.chunk(&token, BASE_UNIT, parts[1]).await;
    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(
        rig.stored_bytes(file_id_of(&json)).await,
        body,
        "被拒的分片污染了已确认区间"
    );

    cleanup(&pool, &[UPLOADER_CONFLICT]).await;
}

/// 没传完就 complete → 拒绝并说明还差多少；abort 之后会话就没了。
#[tokio::test]
async fn completing_early_is_refused_and_abort_drops_the_session() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_ABORT]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(BASE_UNIT * 3, 0x44);
    let token = rig.issue(UPLOADER_ABORT, &body).await;
    rig.chunk(&token, 0, &body[..BASE_UNIT]).await;

    let (code, json) = rig.complete(&token).await;
    assert_ne!(code, StatusCode::OK, "没传完不能完成：{json}");

    let (code, json) = rig.abort(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");

    // 会话没了：status 归零，而不是报错。
    let st = rig.status(&token).await;
    assert_eq!(st["confirmed_bytes"].as_u64().unwrap(), 0);
    assert!(ranges(&st).is_empty());

    cleanup(&pool, &[UPLOADER_ABORT]).await;
}

/// 大文件 + 乱序分片：区间合并与最终拼装都要对。
///
/// 乱序是真实情况（并发上传几片，谁先到不一定），顺序发的话这条路径永远测不到。
#[tokio::test]
async fn out_of_order_chunks_of_a_large_file_assemble_correctly() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_BIG]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(BASE_UNIT * 16 + 777, 0x6b);
    let token = rig.issue(UPLOADER_BIG, &body).await;
    let parts: Vec<(usize, &[u8])> = body
        .chunks(BASE_UNIT)
        .enumerate()
        .map(|(i, p)| (i * BASE_UNIT, p))
        .collect();

    // 先发奇数片，再发偶数片：中间必然出现空洞。
    for (offset, part) in parts.iter().filter(|(o, _)| (o / BASE_UNIT) % 2 == 1) {
        let (code, _) = rig.chunk(&token, *offset, part).await;
        assert_eq!(code, StatusCode::OK);
    }
    let st = rig.status(&token).await;
    assert!(ranges(&st).len() > 1, "这时候应当是好几段离散区间：{st}");
    assert!(!st["complete"].as_bool().unwrap());

    for (offset, part) in parts.iter().filter(|(o, _)| (o / BASE_UNIT) % 2 == 0) {
        let (code, _) = rig.chunk(&token, *offset, part).await;
        assert_eq!(code, StatusCode::OK);
    }
    let st = rig.status(&token).await;
    assert_eq!(
        ranges(&st),
        vec![(0, body.len() as u64)],
        "补齐之后必须合并成完整一段：{st}"
    );

    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(rig.stored_bytes(file_id_of(&json)).await, body);

    cleanup(&pool, &[UPLOADER_BIG]).await;
}
