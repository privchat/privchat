//! 真实后端正向链路验收（第二十轮评审 P1）：在真实 S3 兼容服务上跑通
//! 「真实签发 → part-url → 预签名直传 → status（ListParts）→ complete
//! （If-None-Match + 整文件回读）→ PG 建行」与「abort / 扫描恢复」，
//! 证明数据面正向实现本身可工作——不只有负向门禁有效。
//!
//! 🔴 **能力口径（第二十一轮起）**：本地 MinIO 不支持条件删除（DELETE If-Match
//! 被忽略），启动期探测会告警但不再拒绝启动（运营策略：上传期失败返回错误码）。
//! 本用例经测试钩子 `install_s3_direct` 接线，验证数据面正向链路实现本身可工作；
//! 生产能力验收（判据 33）仍是放行建议依据，由运营监控基于告警决定。
//!
//! 门禁：`PRIVCHAT_S3_LIVE_ENDPOINT` / `_ACCESS_KEY` / `_SECRET_KEY` / `_BUCKET`，
//! 缺失整体跳过。真库卫生：建行后按 file_id 删行，失败路径同样清（同 §8.7 口径）。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures::FutureExt as _;
use sha2::Digest as _;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::rpc::file::request_chunked_upload_token::{
    issue_chunked_upload_token, ChunkedTokenServices,
};
use privchat::service::chunked_upload;
use privchat::service::file_service::{FileService, S3DirectUploadWiring};
use privchat::service::final_object_probe::FinalObjectProbe;
use privchat::service::s3_backend::S3DirectBackend;
use privchat::service::upload_token_service::UploadTokenService;
use privchat_protocol::rpc::FileRequestChunkedUploadTokenRequest;

const UPLOADER: u64 = 9_976_100; // 真库卫生区间 9970000..9980000 内
const PREFIX: &str = "files";

struct LiveEnv {
    endpoint: String,
    access_key: String,
    secret_key: String,
    bucket: String,
    region: String,
    /// 🔴 第二十八轮：寻址方式（`PRIVCHAT_S3_LIVE_ADDRESSING`，腾讯 COS 必须 virtual）。
    addressing: Option<String>,
}

fn live_env() -> Option<LiveEnv> {
    let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (Some(endpoint), Some(access_key), Some(secret_key), Some(bucket)) = (
        get("PRIVCHAT_S3_LIVE_ENDPOINT"),
        get("PRIVCHAT_S3_LIVE_ACCESS_KEY"),
        get("PRIVCHAT_S3_LIVE_SECRET_KEY"),
        get("PRIVCHAT_S3_LIVE_BUCKET"),
    ) else {
        eprintln!("跳过：未配置 PRIVCHAT_S3_LIVE_* 环境变量（正向链路验收未启用）");
        return None;
    };
    Some(LiveEnv {
        endpoint,
        access_key,
        secret_key,
        bucket,
        region: get("PRIVCHAT_S3_LIVE_REGION").unwrap_or_else(|| "us-east-1".to_string()),
        addressing: get("PRIVCHAT_S3_LIVE_ADDRESSING"),
    })
}

/// 与生产 `object_url` 同口径的测试辅助：按寻址方式拼对象 URL。
fn live_object_url(env: &LiveEnv, key: &str) -> String {
    if env.addressing.as_deref() == Some("virtual") {
        let (scheme, host) = env.endpoint.split_once("://").unwrap_or(("https", &env.endpoint));
        format!("{scheme}://{}.{host}/{}", env.bucket, key.trim_start_matches('/'))
    } else {
        format!("{}/{}/{}", env.endpoint.trim_end_matches('/'), env.bucket, key)
    }
}

fn source_config(env: &LiveEnv, id: u32, direct_upload: Option<&str>) -> FileStorageSourceConfig {
    FileStorageSourceConfig {
        id,
        storage_type: if id == 0 { "local" } else { "s3" }.to_string(),
        storage_root: String::new(),
        base_url: None,
        endpoint: Some(env.endpoint.clone()),
        bucket: Some(env.bucket.clone()),
        access_key_id: Some(env.access_key.clone()),
        secret_access_key: Some(env.secret_key.clone()),
        path_prefix: None,
        direct_upload: direct_upload.map(|s| s.to_string()),
        region: Some(env.region.clone()),
        addressing_style: env.addressing.clone(),
    }
}

/// 确认桶可用（第二十八轮，与 `s3_backend_live_test.rs` 同口径）：腾讯 COS 禁止
/// PutBucket 且强制虚拟主机寻址，旧版建桶不适用；桶都是控制台预建的。改为零字节
/// 探针：2xx/403 均证明桶可达且凭证有效；网络层失败立即终止。
async fn ensure_bucket(env: &LiveEnv) {
    let signer = reqsign::AwsV4Signer::new("s3", &env.region);
    let cred = reqsign::AwsCredential {
        access_key_id: env.access_key.clone(),
        secret_access_key: env.secret_key.clone(),
        session_token: None,
        expires_in: None,
    };
    let url = live_object_url(env, "__privchat_probe__/bucket-check");
    let mut req = reqwest::Client::new()
        .put(&url)
        .header("content-length", "0")
        .build()
        .expect("build 探针 PUT");
    signer.sign(&mut req, &cred).expect("sign 探针 PUT");
    let resp = reqwest::Client::new()
        .execute(req)
        .await
        .expect("探针 PUT 请求失败（网络层）");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success() || status == reqwest::StatusCode::FORBIDDEN,
        "桶不可用: HTTP {} body={body}",
        status.as_u16()
    );
}

struct Rig {
    state: FileServerState,
    backend: Arc<S3DirectBackend>,
    pool: Arc<PgPool>,
    config: privchat::config::ServerConfig,
    _dir: tempfile::TempDir,
}

/// 🔴 接线走测试钩子：`direct_upload` 设为 None（生产 init 的能力门禁照常对
/// MinIO 拒绝，本用例不绕过判定逻辑，只是自己把执行体装上跑正向链路）。
async fn make_rig(env: &LiveEnv) -> Rig {
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
    let mut local = source_config(env, 0, None);
    local.storage_root = dir.path().to_string_lossy().to_string();
    local.base_url = Some("http://e2e.local/files".to_string());
    // S3 源 id=1 进源表但不开 direct_upload：建行冻结校验（判据 28）需要它在。
    let s3_source = source_config(env, 1, None);
    let file_service = Arc::new(FileService::new(vec![local, s3_source], 0, pool.clone()));
    file_service.init().await.expect("init storage（无 direct_upload，不触发能力门禁）");
    let backend = Arc::new(S3DirectBackend::from_source(&source_config(env, 1, Some("s3_multipart_v1")))
        .expect("live 配置完整，构建不应失败"));
    file_service.install_s3_direct(S3DirectUploadWiring {
        source_id: 1,
        bucket: env.bucket.clone(),
        path_prefix: PREFIX.to_string(),
        backend: backend.clone(),
        probe: backend.clone(),
    });
    Rig {
        state: FileServerState {
            file_service: file_service.clone(),
            upload_token_service: Arc::new(UploadTokenService::new()),
            auth: None,
            numbered_part_backend: Some(backend.clone()),
            final_object_probe: Some(backend.clone()),
            attachment_keys: test_config().attachment_keys.clone(),
        },
        backend,
        pool,
        config: test_config(),
        _dir: dir,
    }
}

fn services(rig: &Rig) -> ChunkedTokenServices<'_> {
    ChunkedTokenServices {
        // 这些用例验的是分片协商，不涉及加密密钥下发。
        attachment_key: None,
        file_service: &rig.state.file_service,
        upload_token_service: &rig.state.upload_token_service,
        config: &rig.config,
        file_api_base_url: Some("http://e2e.local/files"),
    }
}

/// 单一数据面：S3 接线在位 → 客户端必须声明 s3_multipart_v1（带全两项声明）。
fn req(file_size: i64, plaintext_sha256: &str) -> FileRequestChunkedUploadTokenRequest {
    FileRequestChunkedUploadTokenRequest {
        file_type: "file".to_string(),
        business_type: "message".to_string(),
        plaintext_size: file_size,
        plaintext_sha256: plaintext_sha256.to_string(),
        mime_type: "application/octet-stream".to_string(),
        filename: Some("payload.bin".to_string()),
        force_upload: true,
        supported_upload_transports: Some(vec![
            "proxy_offset_v1".to_string(),
            "s3_multipart_v1".to_string(),
        ]),
    }
}

async fn call(rig: &Rig, request: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = privchat::http::routes::upload::create_route()
        .with_state(rig.state.clone())
        .oneshot(request)
        .await
        .expect("router response");
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.expect("read body");
    let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json envelope");
    (status, json)
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(data))
}

fn part_data(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add((i % 251) as u8)).collect()
}

/// 真库卫生：按 uploader 清行并复查为 0；失败路径（含断言 panic）也必清。
async fn cleanup(pool: &PgPool) {
    sqlx::query("DELETE FROM privchat_file_uploads WHERE uploader_id = $1")
        .bind(UPLOADER as i64)
        .execute(pool)
        .await
        .expect("cleanup DELETE");
    let remaining: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_file_uploads WHERE uploader_id = $1")
            .bind(UPLOADER as i64)
            .fetch_one(pool)
            .await
            .expect("cleanup 复查");
    assert_eq!(remaining, 0, "cleanup 后不得残留行");
}

async fn run_with_cleanup<F>(pool: &PgPool, fut: F)
where
    F: std::future::Future<Output = ()>,
{
    let outcome = std::panic::AssertUnwindSafe(fut).catch_unwind().await;
    cleanup(pool).await;
    if let Err(payload) = outcome {
        std::panic::resume_unwind(payload);
    }
}

// ================= 1. 正向全链路：签发 → part-url → 直传 → status → complete → 建行 =================

/// 几何冻结公式（§8.1）：16 MiB + 100 KiB → part_size=8 MiB、3 片（末片 100 KiB）。
const PART_SIZE: usize = 8 << 20;

/// 🔴 第二十轮评审 P1 正向验收：真实签发（真 CreateMPU）→ 预签名分片直传 →
/// status/ListParts → complete（If-None-Match + 整文件回读摘要）→ PG 建行。
/// 内容随机种子保证每次运行摘要唯一，不命中秒传索引。
/// 签发要按配置冻结加密参数；没有附件密钥时 `freeze_crypto` 直接失败（fail-closed）。
fn test_config() -> privchat::config::ServerConfig {
    privchat::config::ServerConfig {
        attachment_keys: privchat::config::AttachmentKeys(vec![(
            1,
            "WlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlpaWlo".to_string(),
        )]),
        ..privchat::config::ServerConfig::default()
    }
}

#[tokio::test]
async fn live_forward_chain_issue_to_pg_row() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let rig = make_rig(&env).await;
    let pool = rig.pool.clone();
    run_with_cleanup(&pool, async move {
        let seed: u8 = rand::random();
        let mut p1 = part_data(seed, PART_SIZE);
        p1[0] = seed; // 随机种子落首字节，整文件摘要唯一
        let p2 = part_data(seed.wrapping_add(1), PART_SIZE);
        let p3 = part_data(seed.wrapping_add(2), 100 << 10);
        let full: Vec<u8> = p1.iter().chain(&p2).chain(&p3).copied().collect();
        let total = full.len() as i64;
        let sealed = sha256_hex(&full);

        // ① 真实签发：真 CreateMultipartUpload，冻结字段一次写成。
        let resp = issue_chunked_upload_token(&services(&rig), UPLOADER, req(total, &sealed))
            .await
            .expect("真实签发应成功");
        assert_eq!(resp.transport.as_deref(), Some("s3_multipart_v1"));
        let (part_size, total_parts) = chunked_upload::s3_part_geometry(total as u64);
        assert_eq!((part_size, total_parts), (PART_SIZE as u64, 3));
        let token = resp.upload_token.expect("token");
        let upload_id = token.split('.').next().expect("upload id").to_string();

        // ② part-url：批量签发真实预签名（3 片一次）。
        let items: Vec<serde_json::Value> = [&p1, &p2, &p3]
            .iter()
            .enumerate()
            .map(|(i, data)| {
                serde_json::json!({
                    "part_number": i as u32 + 1,
                    "content_length": data.len() as u64,
                    "checksum_sha256_hex": sha256_hex(data),
                })
            })
            .collect();
        let (status, json) = call(
            &rig,
            Request::builder()
                .method("POST")
                .uri("/api/app/files/part-url")
                .header("X-Upload-Token", &token)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::json!({ "parts": items }).to_string()))
                .expect("build part-url"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "part-url 必须放行：{json}");

        // ③ 预签名直传：照抄 required_headers（客户端口径）。
        for (i, data) in [&p1, &p2, &p3].iter().enumerate() {
            let part = &json["data"]["parts"][i];
            let url = part["url"].as_str().expect("url");
            let mut put = reqwest::Client::new().put(url).body(data.to_vec());
            if let Some(headers) = part["required_headers"].as_object() {
                for (k, v) in headers {
                    put = put.header(k.as_str(), v.as_str().expect("header value"));
                }
            }
            let resp = put.send().await.expect("PUT 分片");
            let st = resp.status();
            let body = resp.text().await.unwrap_or_default();
            assert!(st.is_success(), "分片 {} 直传失败: HTTP {} body={body}", i + 1, st.as_u16());
        }

        // ④ status：真实 ListParts 换算，三片齐 → completed。
        let (status, json) = call(
            &rig,
            Request::builder()
                .method("GET")
                .uri("/api/app/files/status")
                .header("X-Upload-Token", &token)
                .body(Body::empty())
                .expect("build status"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["data"]["received_bytes"], total as u64);
        assert_eq!(json["data"]["missing"].as_array().expect("missing").len(), 0);

        // ⑤ complete：If-None-Match complete + 整文件回读摘要核对 + PG 建行。
        let (status, json) = call(
            &rig,
            Request::builder()
                .method("POST")
                .uri("/api/app/files/complete")
                .header("X-Upload-Token", &token)
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"encryption_version":0}"#))
                .expect("build complete"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "complete 必须成功：{json}");
        let file_id: i64 = json["data"]["file_id"].as_i64().expect("file_id");
        assert!(file_id > 0);

        // ⑥ 建行核对 + 对象回读身份一致。
        // 身份字段都在对象行上（引用行只留 object_id）。
        let (row_sha, row_uploader): (String, i64) = sqlx::query_as(
            "SELECT o.sealed_sha256, u.uploader_id FROM privchat_file_uploads u \
             JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
             WHERE u.file_id = $1",
        )
        .bind(file_id)
        .fetch_one(&*rig.pool)
        .await
        .expect("建行必须在");
        assert_eq!(row_sha, sealed, "对象行的密文摘要 = 服务端回读算出的整文件摘要");
        assert_eq!(row_uploader, UPLOADER as i64);

        // 收尾：删 PG 行 + 删桶内对象（对 ETag 条件删；MinIO 上条件被忽略，
        // 这里是测试清理不是安全验收——条件删除安全由判据 29/33 与放行后端保证）。
        sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
            .bind(file_id)
            .execute(&*rig.pool)
            .await
            .expect("删测试行");
        let reference = privchat::service::numbered_parts::UploadReference {
            bucket: env.bucket.clone(),
            final_key: format!("{PREFIX}/{}", manifest_final_key(&rig, &upload_id)),
            provider_upload_id: String::new(),
        };
        if let Ok(Some(head)) = rig.backend.head(&reference).await {
            let _ = rig.backend.delete_if_match(&reference, &head.etag).await;
            assert!(rig.backend.head(&reference).await.unwrap().is_none(), "清理后对象消失");
        }
    })
    .await;
}

fn manifest_final_key(rig: &Rig, upload_id: &str) -> String {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let manifest: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("chunked").join(upload_id).join("manifest.json"))
            .expect("read manifest"),
    )
    .expect("parse manifest");
    manifest["final_key"].as_str().expect("final_key").to_string()
}

// ================= 2. abort：真实 AbortMPU + ListParts 确认 + 目录清理 =================

/// 传一片后 abort：真实 MPU 被清（ListParts → NoSuchUpload）、无 final 对象、
/// 会话目录删除。全程无桶内残留。
#[tokio::test]
async fn live_abort_cleans_real_mpu_and_session() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let rig = make_rig(&env).await;
    let pool = rig.pool.clone();
    run_with_cleanup(&pool, async move {
        let seed: u8 = rand::random();
        let p1 = part_data(seed, PART_SIZE);
        let p2 = part_data(seed.wrapping_add(1), PART_SIZE);
        let full: Vec<u8> = p1.iter().chain(&p2).copied().collect();
        let total = full.len() as i64;
        let resp = issue_chunked_upload_token(&services(&rig), UPLOADER, req(total, &sha256_hex(&full)))
            .await
            .expect("签发");
        let token = resp.upload_token.expect("token");
        let upload_id = token.split('.').next().expect("upload id").to_string();
        // abort 会删会话目录：先读出 final_key 供事后核验。
        let final_key = manifest_final_key(&rig, &upload_id);

        // 只传第 1 片。
        let body = serde_json::json!({ "parts": [{
            "part_number": 1,
            "content_length": p1.len() as u64,
            "checksum_sha256_hex": sha256_hex(&p1),
        }] });
        let (status, json) = call(
            &rig,
            Request::builder()
                .method("POST")
                .uri("/api/app/files/part-url")
                .header("X-Upload-Token", &token)
                .header("Content-Type", "application/json")
                .body(Body::from(body.to_string()))
                .expect("build part-url"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let url = json["data"]["parts"][0]["url"].as_str().expect("url");
        let mut put = reqwest::Client::new().put(url).body(p1.clone());
        if let Some(headers) = json["data"]["parts"][0]["required_headers"].as_object() {
            for (k, v) in headers {
                put = put.header(k.as_str(), v.as_str().expect("header value"));
            }
        }
        assert!(put.send().await.expect("PUT").status().is_success());

        // abort。
        let (status, json) = call(
            &rig,
            Request::builder()
                .method("POST")
                .uri("/api/app/files/abort")
                .header("X-Upload-Token", &token)
                .body(Body::empty())
                .expect("build abort"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "abort 必须成功：{json}");

        // 真实确认：MPU 已彻底关闭，无 final 对象，会话目录删除。
        let reference = privchat::service::numbered_parts::UploadReference {
            bucket: env.bucket.clone(),
            final_key,
            provider_upload_id: String::new(),
        };
        assert!(rig.backend.head(&reference).await.unwrap().is_none(), "abort 不得产生 final 对象");
        let root = rig.state.file_service.upload_session_root().expect("session root");
        assert!(!root.join("chunked").join(&upload_id).exists(), "abort 后会话目录必须删除");
    })
    .await;
}

// ================= 3. 扫描恢复：过期会话经真实接线被扫描器清掉 =================

/// 判据 20 的真实版：签发后不传任何片，把会话改成过期，`sweep_expired_s3`
/// 持生产接线 → 真实 abort（幂等）+ HEAD 空 → 删目录，桶内无残留。
#[tokio::test]
async fn live_scanner_cleans_expired_session_with_real_wiring() {
    let Some(env) = live_env() else { return };
    ensure_bucket(&env).await;
    let rig = make_rig(&env).await;
    let pool = rig.pool.clone();
    run_with_cleanup(&pool, async move {
        let seed: u8 = rand::random();
        let p1 = part_data(seed, PART_SIZE);
        let full = p1.clone();
        let resp = issue_chunked_upload_token(
            &services(&rig),
            UPLOADER,
            req(full.len() as i64, &sha256_hex(&full)),
        )
        .await
        .expect("签发");
        let token = resp.upload_token.expect("token");
        let upload_id = token.split('.').next().expect("upload id").to_string();

        // 改成过期。
        let root = rig.state.file_service.upload_session_root().expect("session root");
        let manifest_path = root.join("chunked").join(&upload_id).join("manifest.json");
        let mut m: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read")).expect("parse");
        m.as_object_mut()
            .expect("object")
            .insert("expires_at".into(), serde_json::json!(1));
        std::fs::write(&manifest_path, m.to_string()).expect("rewrite");

        // 与 server.rs 扫描循环同一口径的接线。
        let wiring = rig.state.file_service.s3_direct().expect("接线必须存在");
        let removed = chunked_upload::sweep_expired_s3(
            &root,
            Some(&wiring.backend),
            Some(&wiring.probe),
            &rig.state.file_service,
        )
        .await;
        assert_eq!(removed, 1, "真实接线下扫描必须清掉过期 S3 会话");
        assert!(!root.join("chunked").join(&upload_id).exists(), "目录必须删除");
        assert!(
            std::fs::read_dir(root.join("s3-anchors")).map(|rd| rd.count()).unwrap_or(0) == 0,
            "锚点不得残留"
        );
    })
    .await;
}
