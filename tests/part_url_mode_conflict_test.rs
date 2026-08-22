//! part-url 串用门禁（RESUMABLE_UPLOAD_SPEC §8.3，实现顺序第 2 步）。
//!
//! 🔴 端点与 transport 强绑定：proxy 会话调 `POST /files/part-url` 必须回
//! `UploadModeConflict`（20616，终局失败）。直传门禁接入前所有会话都是 proxy，
//! 这条门禁就是当前 part-url 的全部可运行行为——盯死它，接入 S3 时才不会
//! 悄悄放行串用。

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use sqlx::postgres::PgPoolOptions;
use tower::ServiceExt;

use privchat::config::FileStorageSourceConfig;
use privchat::http::FileServerState;
use privchat::service::chunked_upload::{ChunkedSession, NewSession};
use privchat::service::file_service::FileService;
use privchat::service::upload_token_service::UploadTokenService;

const UPLOADER: u64 = 9_973_001;

struct Rig {
    state: FileServerState,
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
        addressing_style: None,
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
        _dir: dir,
    }
}

async fn create_proxy_session(rig: &Rig) -> String {
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let (_, token, _) = ChunkedSession::create(
        &root,
        NewSession {
            uploader_id: UPLOADER,
            total_size: 4 << 20,
            sealed_sha256: "ab".repeat(32),
            file_type: "file".into(),
            business_type: "message".into(),
            filename: "payload.bin".into(),
            mime_type: "application/octet-stream".into(),
            transform_version: 0,
            reserved_file_id: 9_973_900,
            transport: "proxy_offset_v1".to_string(),
            s3: None,
        },
    )
    .expect("create session");
    token
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

#[tokio::test]
async fn proxy_session_calling_part_url_gets_upload_mode_conflict() {
    let rig = rig().await;
    let token = create_proxy_session(&rig).await;
    let body = serde_json::json!({
        "parts": [{
            "part_number": 1,
            "content_length": 4194304,
            "checksum_sha256_hex": "0".repeat(64)
        }]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], 20616, "proxy 会话调 part-url 必须回 UploadModeConflict");
}

/// 升级兼容：旧 manifest 没有 transport 字段 → 默认 proxy，同样回 20616，
/// 而不是因为字段缺失走到 500 或放行。
#[tokio::test]
async fn legacy_manifest_without_transport_is_still_proxy() {
    let rig = rig().await;
    let token = create_proxy_session(&rig).await;

    // 模拟升级前的 manifest：把 transport 字段删掉重写。
    let root = rig.state.file_service.upload_session_root().expect("session root");
    let upload_id = token.split('.').next().expect("upload id").to_string();
    let manifest_path = root.join("chunked").join(&upload_id).join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest
        .as_object_mut()
        .expect("object")
        .remove("transport");
    std::fs::write(&manifest_path, manifest.to_string()).expect("rewrite manifest");

    let body = serde_json::json!({
        "parts": [{
            "part_number": 1,
            "content_length": 4194304,
            "checksum_sha256_hex": "0".repeat(64)
        }]
    });
    let (status, json) = post_part_url(&rig, &token, &body.to_string()).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["code"], 20616, "缺 transport 的旧会话按 proxy 处理");
}

/// 没有 token 的 part-url：普通参数/鉴权错误，与其余分片端点口径一致。
#[tokio::test]
async fn part_url_without_token_is_rejected() {
    let rig = rig().await;
    let req = Request::builder()
        .method("POST")
        .uri("/api/app/files/part-url")
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"parts":[]}"#))
        .expect("build request");
    let resp = privchat::http::routes::upload::create_route()
        .with_state(rig.state.clone())
        .oneshot(req)
        .await
        .expect("router response");
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}
