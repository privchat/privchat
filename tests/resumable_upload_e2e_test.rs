// 断点续传的端到端门禁：分片写入 → 断掉 → 查缺口 → 接着传 → 完成。
//
// 🔴 这一套盯的是**功能本身跑不跑得通**，而不是某个函数的返回值：真的 HTTP 请求打进
// 真的路由，字节落到真的磁盘，记录进真的 Postgres，中间用「换一个进程」来模拟服务端
// 重启——重启之后仍然只补缺口，而不是从头再来，这正是断点续传要给用户的东西。

//! 分片上传 E2E（RESUMABLE_UPLOAD_SPEC §7，冻结于 privchat-docs `bdef282`）。
//!
//! 直接打 axum Router，走真实的本地存储与 PostgreSQL。凭据由
//! `ChunkedSession::create` 生成——**与生产 RPC 是同一个函数**，不是夹具另造。

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::service::chunked_upload::{ChunkedSession, NewSession, BASE_UNIT};
use privchat::service::file_service::FileService;
use privchat::service::upload_token_service::UploadTokenService;

const UPLOADER_FLOW: u64 = 9_971_001;
const UPLOADER_RESUME: u64 = 9_971_002;
const UPLOADER_IDEMPOTENT: u64 = 9_971_003;
const UPLOADER_CONFLICT: u64 = 9_971_004;
const UPLOADER_ABORT: u64 = 9_971_005;
const UPLOADER_ORDER: u64 = 9_971_006;
const UPLOADER_REPLAY: u64 = 9_971_007;
const UPLOADER_TOMB: u64 = 9_971_008;

const UNIT: usize = BASE_UNIT as usize;

struct Rig {
    state: FileServerState,
    root: PathBuf,
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

async fn rig(pool: Arc<sqlx::PgPool>) -> Rig {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    rig_at(root, pool, Some(dir)).await
}

/// 「服务端重启」= 丢掉这个 Rig，在同一个根目录上建一个新的。
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
            upload_token_service: Arc::new(UploadTokenService::new()),
            auth: None,
            numbered_part_backend: None,
            final_object_probe: None,
        },
        root,
        _dir: dir,
    }
}

impl Rig {
    fn router(&self) -> axum::Router {
        privchat::http::routes::upload::create_route().with_state(self.state.clone())
    }

    /// 与生产 RPC 同一条构造路径：预留 file_id → `ChunkedSession::create`。
    async fn issue(&self, uid: u64, body: &[u8]) -> String {
        let reserved = self.state.file_service.reserve_file_id().await.expect("reserve");
        let root = self.state.file_service.upload_session_root().expect("root");
        let (_, token, _) = ChunkedSession::create(
            &root,
            NewSession {
                uploader_id: uid,
                total_size: body.len() as u64,
                sealed_sha256: sha256_hex(body),
                file_type: "file".into(),
                business_type: "message".into(),
                filename: "resumable.bin".into(),
                mime_type: "application/octet-stream".into(),
                transform_version: 0,
                reserved_file_id: reserved,
                transport: "proxy_offset_v1".to_string(),
            },
        )
        .expect("create session");
        token
    }

    fn session_dir(&self, token: &str) -> PathBuf {
        let root = self.state.file_service.upload_session_root().expect("root");
        ChunkedSession::open(&root, token).expect("open").dir().to_path_buf()
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

    async fn chunk(&self, token: &str, offset: usize, bytes: &[u8]) -> (StatusCode, serde_json::Value) {
        self.chunk_with_digest(token, offset, bytes, &sha256_hex(bytes)).await
    }

    async fn chunk_with_digest(
        &self,
        token: &str,
        offset: usize,
        bytes: &[u8],
        digest: &str,
    ) -> (StatusCode, serde_json::Value) {
        self.send(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/app/files/chunk?offset={offset}"))
                .header("X-Upload-Token", token)
                .header("X-Chunk-SHA256", digest)
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
                .body(Body::from(r#"{"encryption_version":0}"#))
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

    async fn rows_of(&self, pool: &sqlx::PgPool, uid: u64) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM privchat_file_uploads WHERE uploader_id = $1",
        )
        .bind(uid as i64)
        .fetch_one(pool)
        .await
        .expect("count")
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))
}

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

fn ranges(v: &serde_json::Value) -> Vec<(u64, u64)> {
    v.as_array()
        .expect("ranges")
        .iter()
        .map(|r| (r["offset"].as_u64().unwrap(), r["length"].as_u64().unwrap()))
        .collect()
}

fn code_of(json: &serde_json::Value) -> u64 {
    json["code"].as_u64().unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------- 用例

/// 判据 1：全新文件 → parts/ 出现分片 → complete → 逐字节一致。
#[tokio::test]
async fn a_chunked_upload_completes_and_matches_byte_for_byte() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_FLOW]).await;
    let rig = rig(pool.clone()).await;

    let body = payload(UNIT * 3 + 12_345, 0x5a);
    let token = rig.issue(UPLOADER_FLOW, &body).await;
    let dir = rig.session_dir(&token);

    for (i, part) in body.chunks(UNIT).enumerate() {
        let (code, json) = rig.chunk(&token, i * UNIT, part).await;
        assert_eq!(code, StatusCode::OK, "第 {i} 片失败：{json}");
        assert_eq!(json["data"]["outcome"], "written");
        assert!(dir.join("parts").join(format!("{}-{}.part", i * UNIT, part.len())).exists());
    }

    let st = rig.status(&token).await;
    assert_eq!(st["received_bytes"].as_u64().unwrap(), body.len() as u64);
    assert_eq!(ranges(&st["received"]), vec![(0, body.len() as u64)], "应当合并成一段");
    assert!(ranges(&st["missing"]).is_empty());

    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    let file_id = file_id_of(&json);
    assert_eq!(rig.stored_bytes(file_id).await, body, "落盘内容必须逐字节相同");
    // 墓碑在、parts 没了。
    assert!(dir.join("completed.json").exists());
    assert!(!dir.join("parts").exists());

    cleanup(&pool, &[UPLOADER_FLOW]).await;
}

/// 判据 3：传一半 → 服务端「重启」→ status 只回缺失 → 只补缺失 → 完成；线上总字节 = 文件大小。
#[tokio::test]
async fn an_interrupted_upload_resumes_from_the_gap_after_a_restart() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_RESUME]).await;
    let keep = tempfile::tempdir().expect("tempdir");
    let root = keep.path().to_path_buf();
    let body = payload(UNIT * 4, 0x11);

    let token = {
        let rig = rig_at(root.clone(), pool.clone(), None).await;
        let token = rig.issue(UPLOADER_RESUME, &body).await;
        for i in [0usize, 2] {
            let (code, json) = rig.chunk(&token, i * UNIT, &body[i * UNIT..(i + 1) * UNIT]).await;
            assert_eq!(code, StatusCode::OK, "{json}");
        }
        token
    };

    let rig = rig_at(root, pool.clone(), None).await;
    let st = rig.status(&token).await;
    let missing = ranges(&st["missing"]);
    assert_eq!(missing, vec![(UNIT as u64, UNIT as u64), (3 * UNIT as u64, UNIT as u64)]);
    assert_eq!(ranges(&st["received"]), vec![(0, UNIT as u64), (2 * UNIT as u64, UNIT as u64)]);

    let mut sent = 2 * UNIT as u64;
    for (off, len) in missing {
        let (code, json) = rig
            .chunk(&token, off as usize, &body[off as usize..(off + len) as usize])
            .await;
        assert_eq!(code, StatusCode::OK, "{json}");
        sent += len;
    }
    assert_eq!(sent, body.len() as u64, "多一个字节，省下的带宽就是假的");

    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(rig.stored_bytes(file_id_of(&json)).await, body);
    cleanup(&pool, &[UPLOADER_RESUME]).await;
}

/// 判据 4：同一片重复上传 → 幂等成功、磁盘不变；同边界不同内容 → 409，磁盘不变。
#[tokio::test]
async fn resending_the_same_chunk_is_idempotent_and_other_bytes_conflict() {
    let Some(pool) = pool().await else { return };
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT * 2, 0x22);
    let token = rig.issue(UPLOADER_IDEMPOTENT, &body).await;
    let dir = rig.session_dir(&token);
    let part = dir.join("parts").join(format!("0-{UNIT}.part"));

    let (code, json) = rig.chunk(&token, 0, &body[..UNIT]).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    let mtime = std::fs::metadata(&part).unwrap().modified().unwrap();
    let (code, json) = rig.chunk(&token, 0, &body[..UNIT]).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(json["data"]["outcome"], "already_present");
    assert_eq!(std::fs::metadata(&part).unwrap().modified().unwrap(), mtime, "磁盘不能动");

    let other = payload(UNIT, 0x99);
    let (code, json) = rig.chunk(&token, 0, &other).await;
    assert_eq!(code, StatusCode::CONFLICT, "{json}");
    assert_eq!(code_of(&json), 20610);
    assert_eq!(std::fs::read(&part).unwrap(), &body[..UNIT]);
    // 边界重叠不相同（跨两片的一段）也拒。
    let (code, json) = rig.chunk(&token, UNIT / 2 * 0, &body[..UNIT + UNIT]).await;
    assert_eq!(code, StatusCode::CONFLICT, "{json}");
    assert_eq!(std::fs::read_dir(dir.join("parts")).unwrap().count(), 1);
}

/// 判据 5：乱序上传 → complete 仍拼对。
#[tokio::test]
async fn out_of_order_chunks_still_assemble() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_ORDER]).await;
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT * 3 + 777, 0x33);
    let token = rig.issue(UPLOADER_ORDER, &body).await;
    let mut pieces: Vec<(usize, &[u8])> = body.chunks(UNIT).enumerate().map(|(i, c)| (i * UNIT, c)).collect();
    pieces.reverse();
    for (off, part) in pieces {
        let (code, json) = rig.chunk(&token, off, part).await;
        assert_eq!(code, StatusCode::OK, "{json}");
    }
    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{json}");
    assert_eq!(rig.stored_bytes(file_id_of(&json)).await, body);
    cleanup(&pool, &[UPLOADER_ORDER]).await;
}

/// 判据 7：complete 成功后重放 → 同一个 file_id、表不多一行。
#[tokio::test]
async fn replaying_complete_returns_the_same_file_id() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_REPLAY]).await;
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT + 5, 0x44);
    let token = rig.issue(UPLOADER_REPLAY, &body).await;
    for (i, part) in body.chunks(UNIT).enumerate() {
        rig.chunk(&token, i * UNIT, part).await;
    }
    let (_, first) = rig.complete(&token).await;
    let id = file_id_of(&first);
    let (code, again) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{again}");
    assert_eq!(file_id_of(&again), id);
    assert_eq!(rig.rows_of(&pool, UPLOADER_REPLAY).await, 1);
    // status 也要报告 completed。
    let st = rig.status(&token).await;
    assert_eq!(st["completed"], true);
    // 迟到的分片：拒。
    let (code, json) = rig.chunk(&token, 0, &body[..UNIT]).await;
    assert_eq!(code, StatusCode::CONFLICT, "{json}");
    assert_eq!(code_of(&json), 20614);
    // 已完成不能 abort。
    let (code, _) = rig.abort(&token).await;
    assert_eq!(code, StatusCode::CONFLICT);
    cleanup(&pool, &[UPLOADER_REPLAY]).await;
}

/// 判据 8（进程内版）：PG 已提交、墓碑没写 → 重试 complete → 同 id、不多行、对象不重写。
///
/// 这里模拟的是「墓碑丢了」这一态：把 completed.json 删掉，parts 也没了——恢复必须
/// 靠 manifest 里的 `reserved_file_id` 查到正式行。真正的 SIGKILL 注入见
/// `PRIVCHAT_CRASH_POINT=after_commit_before_tombstone`。
#[tokio::test]
async fn a_lost_tombstone_is_recovered_through_the_reserved_file_id() {
    let Some(pool) = pool().await else { return };
    cleanup(&pool, &[UPLOADER_TOMB]).await;
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT * 2, 0x55);
    let token = rig.issue(UPLOADER_TOMB, &body).await;
    for (i, part) in body.chunks(UNIT).enumerate() {
        rig.chunk(&token, i * UNIT, part).await;
    }
    let (_, first) = rig.complete(&token).await;
    let id = file_id_of(&first);
    let dir = rig.session_dir(&token);
    std::fs::remove_file(dir.join("completed.json")).unwrap();
    let meta = rig.state.file_service.get_file_metadata(id).await.unwrap().unwrap();
    let obj = rig.root.join(meta.file_path.trim_start_matches('/'));
    let mtime = std::fs::metadata(&obj).unwrap().modified().unwrap();

    let (code, again) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::OK, "{again}");
    assert_eq!(file_id_of(&again), id);
    assert_eq!(rig.rows_of(&pool, UPLOADER_TOMB).await, 1);
    assert_eq!(std::fs::metadata(&obj).unwrap().modified().unwrap(), mtime, "对象不得重写");
    assert!(dir.join("completed.json").exists(), "墓碑要补回来");
    cleanup(&pool, &[UPLOADER_TOMB]).await;
}

/// 没传完就 complete → 409 + 缺失区间；abort 删目录；之后所有请求 SessionGone；abort 幂等。
#[tokio::test]
async fn completing_early_is_refused_and_abort_drops_the_session() {
    let Some(pool) = pool().await else { return };
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT * 2, 0x66);
    let token = rig.issue(UPLOADER_ABORT, &body).await;
    rig.chunk(&token, 0, &body[..UNIT]).await;
    let (code, json) = rig.complete(&token).await;
    assert_eq!(code, StatusCode::CONFLICT, "{json}");
    assert_eq!(code_of(&json), 20615);

    let dir = rig.session_dir(&token);
    let (code, _) = rig.abort(&token).await;
    assert_eq!(code, StatusCode::OK);
    assert!(!dir.exists());
    let (code, json) = rig
        .send(
            Request::builder()
                .method("GET")
                .uri("/api/app/files/status")
                .header("X-Upload-Token", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await;
    assert_eq!(code, StatusCode::GONE, "{json}");
    assert_eq!(code_of(&json), 20613);
    let (code, _) = rig.abort(&token).await;
    assert_eq!(code, StatusCode::OK, "abort 幂等");
}

/// 单凭据：错 secret 与不存在同一句话；坏摘要当场 422 且不落盘；不对齐 400。
#[tokio::test]
async fn bad_credentials_and_bad_chunks_are_refused_up_front() {
    let Some(pool) = pool().await else { return };
    let rig = rig(pool.clone()).await;
    let body = payload(UNIT * 2, 0x77);
    let token = rig.issue(UPLOADER_CONFLICT, &body).await;
    let dir = rig.session_dir(&token);

    let (id, _) = token.split_once('.').unwrap();
    let forged = format!("{id}.{}", "f".repeat(64));
    let (code, json) = rig.chunk(&forged, 0, &body[..UNIT]).await;
    assert_eq!(code, StatusCode::GONE, "{json}");
    assert_eq!(code_of(&json), 20613);

    let (code, json) = rig.chunk_with_digest(&token, 0, &body[..UNIT], &"0".repeat(64)).await;
    assert_eq!(code, StatusCode::UNPROCESSABLE_ENTITY, "{json}");
    assert_eq!(code_of(&json), 20611);
    assert_eq!(std::fs::read_dir(dir.join("parts")).unwrap().count(), 0, "坏字节不许碰磁盘");

    let (code, json) = rig.chunk(&token, 1, &body[1..UNIT]).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(code_of(&json), 20617);
    let (code, json) = rig.chunk(&token, 0, &body[..100]).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "非末段不整格：{json}");
    let (code, json) = rig.chunk(&token, UNIT * 2, &body[..10]).await;
    assert_eq!(code, StatusCode::BAD_REQUEST, "越界：{json}");
}
