mod common;
use axum::{
    routing::{get, put},
    Router,
};
use privchat::{
    auth::ServiceKeyManager,
    http::routes::components::{get_components, update_rtc_component, ComponentConfigState},
    rpc::{components, RpcContext},
    service::{
        call_config::CallConfig,
        call_service::{components::CONFIG_LOCK, CallService},
    },
};
use privchat_protocol::rpc::call::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::sync::Arc;

struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn mutation(command: &str, call_id: u64) -> CallMutationRequest {
    CallMutationRequest {
        command_id: command.into(),
        call_id,
        supports_peer: false,
    }
}
fn invite(command: &str) -> CallInviteRequest {
    CallInviteRequest {
        command_id: command.into(),
        channel_id: 200,
        media_kind: CallMediaKind::Audio,
        invitee_ids: vec![2],
        supports_peer: false,
    }
}

#[tokio::test]
async fn managed_components_real_pg_http_runtime_and_replay() {
    let prefix = format!("COMPONENT_TEST_{}", uuid::Uuid::new_v4().simple());
    let key_env = format!("{prefix}_KEY");
    let secret_env = format!("{prefix}_SECRET");
    let db = common::ScratchDatabase::create("components")
        .await
        .expect("isolated DB required");
    let mut conn = db.connect().await;
    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .unwrap();
    drop(conn);
    let pool = PgPool::connect(&db.url()).await.unwrap();
    sqlx::query(
        "INSERT INTO privchat_users(user_id,username,qr_key) VALUES(1,'cc1','cc1'),(2,'cc2','cc2')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO privchat_groups(group_id,name,owner_id,qr_key) VALUES(200,'components',1,'ccg')").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO privchat_channels(channel_id,channel_type,group_id) VALUES(200,1,200)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO privchat_group_members(group_id,user_id,role) VALUES(200,1,0),(200,2,2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let service = CallService::new(
        pool.clone(),
        CallConfig {
            enabled: true,
            livekit_url: "ws://127.0.0.1:57644".into(),
            api_key_env: key_env.clone(),
            api_secret_env: secret_env.clone(),
            ..Default::default()
        },
        Some(("local-key".into(), "local-secret-not-production".into())),
    )
    .unwrap();
    let local = components::get(&service, &RpcContext::new().with_user_id("1".into()))
        .await
        .unwrap();
    assert_eq!(
        local["components"]["rtc"]["livekitUrl"],
        "ws://127.0.0.1:57644"
    );
    assert!(components::get(&service, &RpcContext::new()).await.is_err());
    assert_eq!(local.as_object().unwrap().len(), 4);
    assert_eq!(local["components"]["rtc"].as_object().unwrap().len(), 5);
    assert!(!local.to_string().contains("Env"));
    assert!(!local.to_string().contains("secret"));
    let router = privchat::rpc::router::RpcRouter::new();
    components::register(&router, Arc::new(service.clone())).await;
    let req = privchat::rpc::types::RPCMessageRequest {
        route: "config/get".into(),
        body: json!({"privileged":true}),
    };
    let denied = router.handle(req.clone(), RpcContext::new()).await;
    assert_eq!(
        denied.code,
        privchat_protocol::ErrorCode::AuthRequired.code() as i32
    );
    assert!(denied.data.is_none());
    let allowed = router
        .handle(req, RpcContext::new().with_user_id("1".into()))
        .await;
    assert_eq!(allowed.code, 0);
    assert_eq!(allowed.data.unwrap(), local);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/api/service/config/components", get(get_components))
        .route(
            "/api/service/config/components/rtc",
            put(update_rtc_component),
        )
        .with_state(ComponentConfigState {
            service_key_manager: Arc::new(ServiceKeyManager::new_master_key(
                "test-service-only".into(),
            )),
            service: Arc::new(service.clone()),
        });
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let http = reqwest::Client::new();
    let path = format!("{base}/api/service/config/components");
    let update = format!("{path}/rtc");
    for method in [reqwest::Method::GET, reqwest::Method::PUT] {
        let url = if method == reqwest::Method::GET {
            &path
        } else {
            &update
        };
        for key in [None, Some("wrong-key"), Some("ordinary-user-token")] {
            let mut req = http
                .request(method.clone(), url)
                .bearer_auth("ordinary-user-token");
            if let Some(key) = key {
                req = req.header("X-Service-Key", key);
            }
            let res = req.body("invalid JSON").send().await.unwrap();
            assert_eq!(res.status(), 401);
            let body: Value = res.json().await.unwrap();
            assert!(body["data"].is_null());
            assert!(!body.to_string().contains("local-key"));
        }
    }
    let privileged: Value = http
        .get(&path)
        .header("X-Service-Key", "test-service-only")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(privileged["data"]["revision"], local["revision"]);
    assert!(privileged["data"]["settings"]["apiKeyEnv"].is_string());
    std::env::set_var(&key_env, "managed-test-key");
    std::env::set_var(&secret_env, "managed-test-secret-thirty-two-bytes-minimum");
    let media_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let media_url = format!("ws://{}", media_listener.local_addr().unwrap());
    let cleanup_seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = cleanup_seen.clone();
    let media_app = Router::new().route(
        "/twirp/livekit.RoomService/DeleteRoom",
        axum::routing::post(move |headers: axum::http::HeaderMap| {
            let captured = captured.clone();
            async move {
                captured
                    .lock()
                    .unwrap()
                    .push(headers["authorization"].to_str().unwrap().to_owned());
                axum::Json(json!({}))
            }
        }),
    );
    let _media_server = Server(tokio::spawn(async move {
        axum::serve(media_listener, media_app).await.unwrap();
    }));
    let mut body = json!({"expectedRevision":local["revision"],"settings":{
        "enabled":true,"livekitUrl":media_url,"apiKeyEnv":key_env,"apiSecretEnv":secret_env}});
    for (field, value) in [
        ("livekitUrl", "https://host.invalid"),
        ("livekitUrl", "ws://external.example.invalid:80"),
        ("livekitUrl", "ws://192.168.1.1:80"),
        ("livekitUrl", "ws://localhost"),
        ("livekitUrl", "ws://localhost.evil:80"),
        ("livekitUrl", "wss://host.invalid:0"),
        ("livekitUrl", "ws://user:secret@host.invalid"),
        ("livekitUrl", "wss://host.invalid?token=secret"),
        ("livekitUrl", "wss://host.invalid/#secret"),
        ("apiKeyEnv", "bad-name"),
        ("apiSecretEnv", "COMPONENT_TEST_MISSING_NO_VALUE"),
    ] {
        let mut bad = body.clone();
        bad["settings"][field] = json!(value);
        let res = http
            .put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&bad)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400, "{field} {value}");
    }
    let mut extra = body.clone();
    extra["settings"]["allowTurn"] = json!(true);
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&extra)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    // A creator holding the shared lock cannot race with configuration application.
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(CONFIG_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    tx.rollback().await.unwrap();
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(CONFIG_LOCK)
        .execute(&mut *tx)
        .await
        .unwrap();
    let pending = tokio::spawn({
        let service = service.clone();
        async move { service.invite(1, "caller", invite("atomic-create")).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert!(
        !pending.is_finished(),
        "creator must wait for updater's lock"
    );
    tx.rollback().await.unwrap();
    let ringing = pending.await.unwrap().unwrap();
    let id = ringing["call"]["call_id"].as_u64().unwrap();
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    assert_eq!(
        service
            .invite(1, "caller", invite("atomic-create"))
            .await
            .unwrap(),
        ringing
    );
    service
        .mutate("cancel", 1, "caller", mutation("cancel", id))
        .await
        .unwrap();
    // Pending SFU cleanup still fences application, even after the call ends.
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    sqlx::query("UPDATE privchat_call_effects SET completed=true")
        .execute(&pool)
        .await
        .unwrap();
    let res = http
        .put(&update)
        .header("X-Service-Key", "test-service-only")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let applied: Value = res.json().await.unwrap();
    let mut revision = applied["data"]["revision"].clone();
    assert_ne!(revision, local["revision"]);
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409,
        "stale/replayed update must not apply twice"
    );
    // A new service instance (restart), still constructed with the old local config, reads durable state.
    let restarted = CallService::new(
        pool.clone(),
        service.config.clone(),
        Some(("local-key".into(), "local-secret-not-production".into())),
    )
    .unwrap();
    let mut concurrent = body.clone();
    concurrent["expectedRevision"] = revision.clone();
    let first = http
        .put(&update)
        .header("X-Service-Key", "test-service-only")
        .json(&concurrent);
    let second = http
        .put(&update)
        .header("X-Service-Key", "test-service-only")
        .json(&concurrent);
    let (first, second) = tokio::join!(first.send(), second.send());
    let mut successes = 0;
    for result in [first.unwrap(), second.unwrap()] {
        if result.status() == 200 {
            successes += 1;
            let value: Value = result.json().await.unwrap();
            revision = value["data"]["revision"].clone();
        } else {
            assert_eq!(result.status(), 409);
        }
    }
    assert_eq!(successes, 1, "same expectedRevision can commit only once");
    let view = components::get(&restarted, &RpcContext::new().with_user_id("2".into()))
        .await
        .unwrap();
    assert_eq!(view["revision"], revision);
    assert_eq!(view["components"]["rtc"]["livekitUrl"], media_url);
    assert_eq!(view["components"]["rtc"]["allowTurn"], false);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "managed_component_reload_child",
            "--nocapture",
        ])
        .env("COMPONENT_RELOAD_DATABASE", db.url())
        .env("COMPONENT_RELOAD_REVISION", revision.as_str().unwrap())
        .env("COMPONENT_RELOAD_URL", &media_url)
        .env("COMPONENT_RELOAD_KEY_REF", &key_env)
        .env("COMPONENT_RELOAD_SECRET_REF", &secret_env)
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "fresh-process durable reload failed: {}",
        String::from_utf8_lossy(&child.stderr)
    );
    let created = restarted
        .invite(1, "caller", invite("managed-invite"))
        .await
        .unwrap();
    let id = created["call"]["call_id"].as_u64().unwrap();
    let joined = restarted
        .mutate("join", 2, "callee", mutation("managed-join", id))
        .await
        .unwrap();
    assert_eq!(joined["url"], media_url);
    let mut validation = jsonwebtoken::Validation::default();
    validation.validate_aud = false;
    let claims = jsonwebtoken::decode::<Value>(
        joined["token"].as_str().unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(b"managed-test-secret-thirty-two-bytes-minimum"),
        &validation,
    )
    .unwrap();
    assert_eq!(claims.claims["iss"], "managed-test-key");
    assert_eq!(
        restarted
            .mutate("join", 2, "callee", mutation("managed-join", id))
            .await
            .unwrap(),
        joined
    );
    let mut reused = invite("managed-invite");
    reused.media_kind = CallMediaKind::Video;
    assert!(restarted.invite(1, "caller", reused).await.is_err());
    body["expectedRevision"] = revision.clone();
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409
    );
    let ended = restarted
        .mutate("end", 1, "caller", mutation("end-managed", id))
        .await
        .unwrap();
    let snapshot: CallSnapshot = serde_json::from_value(ended["call"].clone()).unwrap();
    restarted.media_cleanup(&snapshot, None).await.unwrap();
    let auth = cleanup_seen.lock().unwrap()[0].clone();
    let cleanup_claims = jsonwebtoken::decode::<Value>(
        auth.strip_prefix("Bearer ").unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(b"managed-test-secret-thirty-two-bytes-minimum"),
        &validation,
    )
    .unwrap();
    assert_eq!(cleanup_claims.claims["iss"], "managed-test-key");
    sqlx::query("UPDATE privchat_call_effects SET completed=true WHERE kind IN ('delete_room','remove_participant')")
        .execute(&pool).await.unwrap();
    body["settings"]["enabled"] = json!(false);
    assert_eq!(
        http.put(&update)
            .header("X-Service-Key", "test-service-only")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        409,
        "disabling must not strand terminal state/history effects"
    );
    sqlx::query("UPDATE privchat_call_effects SET completed=true")
        .execute(&pool)
        .await
        .unwrap();
    body["settings"]["enabled"] = json!(false);
    let disabled: Value = http
        .put(&update)
        .header("X-Service-Key", "test-service-only")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(disabled["code"], 0);
    assert_eq!(disabled["data"]["components"]["rtc"]["enabled"], false);
    assert!(restarted
        .invite(1, "caller", invite("disabled-invite"))
        .await
        .is_err());
    body["settings"]["enabled"] = json!(true);
    body["expectedRevision"] = disabled["data"]["revision"].clone();
    let reenabled: Value = http
        .put(&update)
        .header("X-Service-Key", "test-service-only")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(reenabled["code"], 0);
    assert!(restarted.runtime().await.unwrap().capabilities().enabled);
    let stored: Value =
        sqlx::query_scalar("SELECT settings FROM privchat_component_config WHERE component='rtc'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(!stored.to_string().contains("managed-test-secret"));
    assert!(!stored.to_string().contains("managed-test-key"));
    std::env::remove_var(&key_env);
    std::env::remove_var(&secret_env);
    assert!(
        components::get(&restarted, &RpcContext::new().with_user_id("2".into()))
            .await
            .is_err(),
        "missing runtime credentials must not advertise stale success after restart"
    );
    pool.close().await;
}

#[tokio::test]
async fn managed_disabled_blank_defaults_round_trip_and_enable_is_strict() {
    let db = common::ScratchDatabase::create("components_disabled_defaults")
        .await
        .unwrap();
    let mut conn = db.connect().await;
    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .unwrap();
    drop(conn);
    let pool = PgPool::connect(&db.url()).await.unwrap();
    let local = CallConfig {
        api_key_env: format!("UNSET_ALLOWED_KEY_{}", uuid::Uuid::new_v4().simple()),
        api_secret_env: format!("UNSET_ALLOWED_SECRET_{}", uuid::Uuid::new_v4().simple()),
        ..Default::default()
    };
    let service = CallService::configured(pool.clone(), local.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/service/config/components",
        listener.local_addr().unwrap()
    );
    let app = Router::new()
        .route("/api/service/config/components", get(get_components))
        .route(
            "/api/service/config/components/rtc",
            put(update_rtc_component),
        )
        .with_state(ComponentConfigState {
            service_key_manager: Arc::new(ServiceKeyManager::new_master_key(
                "disabled-defaults-service-key".into(),
            )),
            service: Arc::new(service.clone()),
        });
    let _server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let http = reqwest::Client::new();
    let old = service.component_view(true).await.unwrap();
    let settings = json!({"enabled":false,"livekitUrl":"","apiKeyEnv":"","apiSecretEnv":""});
    let mut body = json!({"expectedRevision":old["revision"], "settings":settings});
    let response = http
        .put(format!("{base}/rtc"))
        .header("X-Service-Key", "disabled-defaults-service-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let applied: Value = response.json().await.unwrap();
    assert_eq!(applied["data"]["settings"], settings);
    assert_eq!(applied["data"]["components"]["rtc"]["enabled"], false);
    assert_eq!(applied["data"]["components"]["rtc"]["livekitUrl"], "");
    let revision = applied["data"]["revision"].clone();
    assert_ne!(revision, old["revision"]);
    let restarted = CallService::configured(
        pool.clone(),
        CallConfig {
            livekit_url: "wss://local-fallback.invalid".into(),
            ..Default::default()
        },
    );
    let read: Value = http
        .get(&base)
        .header("X-Service-Key", "disabled-defaults-service-key")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(read["data"], applied["data"]);
    let public = components::get(&restarted, &RpcContext::new().with_user_id("1".into()))
        .await
        .unwrap();
    assert_eq!(public["revision"], revision);
    assert_eq!(public["components"]["rtc"]["livekitUrl"], "");
    assert!(public.get("settings").is_none());
    let runtime = restarted.runtime().await.unwrap();
    assert!(!runtime.capabilities().enabled);
    assert!(
        runtime.config.livekit_url.is_empty(),
        "blank managed URL must not inherit local URL"
    );
    assert!(runtime.config.api_key_env.is_empty());
    assert!(runtime.config.api_secret_env.is_empty());
    body["expectedRevision"] = revision.clone();
    body["settings"]["enabled"] = json!(true);
    let bad = http
        .put(format!("{base}/rtc"))
        .header("X-Service-Key", "disabled-defaults-service-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        bad.status(),
        400,
        "enable must not accept blank dependencies"
    );
    assert_eq!(service.component_view(true).await.unwrap(), applied["data"]);
    body["settings"] = json!({"enabled":false,"livekitUrl":"wss://staged.example.invalid",
        "apiKeyEnv":local.api_key_env,
        "apiSecretEnv":local.api_secret_env});
    let staged: Value = http
        .put(format!("{base}/rtc"))
        .header("X-Service-Key", "disabled-defaults-service-key")
        .json(&body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        staged["code"], 0,
        "disabled settings do not resolve credentials"
    );
    body["expectedRevision"] = staged["data"]["revision"].clone();
    body["settings"]["enabled"] = json!(true);
    assert_eq!(
        http.put(format!("{base}/rtc"))
            .header("X-Service-Key", "disabled-defaults-service-key")
            .json(&body)
            .send()
            .await
            .unwrap()
            .status(),
        400,
        "enable must resolve staged env refs"
    );
    assert_eq!(service.component_view(true).await.unwrap(), staged["data"]);
    pool.close().await;
}

// Invoked explicitly in a fresh process by the real-PG acceptance above.
#[tokio::test]
#[ignore]
async fn managed_component_reload_child() {
    let pool = PgPool::connect(&std::env::var("COMPONENT_RELOAD_DATABASE").unwrap())
        .await
        .unwrap();
    let service = CallService::configured(
        pool.clone(),
        CallConfig {
            api_key_env: std::env::var("COMPONENT_RELOAD_KEY_REF").unwrap(),
            api_secret_env: std::env::var("COMPONENT_RELOAD_SECRET_REF").unwrap(),
            ..Default::default()
        },
    );
    let view = components::get(&service, &RpcContext::new().with_user_id("1".into()))
        .await
        .unwrap();
    assert_eq!(
        view["revision"],
        std::env::var("COMPONENT_RELOAD_REVISION").unwrap()
    );
    assert_eq!(
        view["components"]["rtc"]["livekitUrl"],
        std::env::var("COMPONENT_RELOAD_URL").unwrap()
    );
    assert!(service.runtime().await.unwrap().capabilities().enabled);
    pool.close().await;
}

async fn component_test_pool(name: &str) -> (common::ScratchDatabase, PgPool) {
    let db = common::ScratchDatabase::create(name).await.unwrap();
    let mut conn = db.connect().await;
    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .unwrap();
    drop(conn);
    let pool = PgPool::connect(&db.url()).await.unwrap();
    (db, pool)
}
async fn component_test_http(service: &CallService) -> (String, Server) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!(
        "http://{}/api/service/config/components",
        listener.local_addr().unwrap()
    );
    let app = Router::new()
        .route("/api/service/config/components", get(get_components))
        .route(
            "/api/service/config/components/rtc",
            put(update_rtc_component),
        )
        .with_state(ComponentConfigState {
            service_key_manager: Arc::new(ServiceKeyManager::new_master_key(
                "component-review-key".into(),
            )),
            service: Arc::new(service.clone()),
        });
    let server = Server(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    (base, server)
}
async fn apply_review(http: &reqwest::Client, base: &str, body: &Value) -> reqwest::Response {
    http.put(format!("{base}/rtc"))
        .header("X-Service-Key", "component-review-key")
        .json(body)
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn managed_peer_only_apply_capabilities_invite_and_no_turn() {
    let (_db, pool) = component_test_pool("components_peer_only").await;
    sqlx::query(
        "INSERT INTO privchat_users(user_id,username,qr_key) VALUES(1,'mp1','mp1'),(2,'mp2','mp2')",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO privchat_channels(channel_id,channel_type,direct_user1_id,direct_user2_id) VALUES(100,0,1,2)").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO privchat_channel_participants(channel_id,user_id) VALUES(100,1),(100,2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let local = CallConfig {
        livekit_url: "wss://must-not-be-inherited.invalid".into(),
        api_key_env: format!("UNSET_PEER_KEY_{}", uuid::Uuid::new_v4().simple()),
        api_secret_env: format!("UNSET_PEER_SECRET_{}", uuid::Uuid::new_v4().simple()),
        peer: privchat::service::call_config::CallPeerSettings {
            enabled: true,
            allow_turn: false,
            turn_secret_env: format!("UNSET_PEER_TURN_{}", uuid::Uuid::new_v4().simple()),
            ..Default::default()
        },
        ..Default::default()
    };
    let service = CallService::configured(pool.clone(), local.clone());
    let (base, _http_server) = component_test_http(&service).await;
    let http = reqwest::Client::new();
    let old = service.component_view(true).await.unwrap();
    let settings = json!({"enabled":true,"livekitUrl":"","apiKeyEnv":"","apiSecretEnv":""});
    let mut body = json!({"expectedRevision":old["revision"],"settings":settings});
    let response = apply_review(&http, &base, &body).await;
    assert_eq!(response.status(), 200);
    let applied: Value = response.json().await.unwrap();
    assert_eq!(applied["data"]["runtimeAvailable"], true);
    assert_eq!(applied["data"]["settings"], settings);
    assert_eq!(
        applied["data"]["components"]["rtc"],
        json!({"schemaVersion":1,"enabled":true,"livekitUrl":"","peerEnabled":true,"allowTurn":false})
    );
    let public = components::get(&service, &RpcContext::new().with_user_id("1".into()))
        .await
        .unwrap();
    assert_eq!(public["components"], applied["data"]["components"]);
    assert!(public.get("runtimeAvailable").is_none());
    assert!(public.get("settings").is_none());
    let runtime = service.runtime().await.unwrap();
    assert!(runtime.capabilities().enabled && runtime.capabilities().peer_enabled);
    assert!(!runtime.capabilities().livekit_enabled);
    assert!(runtime.config.livekit_url.is_empty());
    body["expectedRevision"] = applied["data"]["revision"].clone();
    // A nonblank SFU URL still requires its role-approved credentials.
    let mut invalid = body.clone();
    invalid["settings"]["livekitUrl"] = json!("wss://rtc.example.invalid");
    assert_eq!(apply_review(&http, &base, &invalid).await.status(), 400);
    invalid["settings"]["apiKeyEnv"] = json!(local.api_key_env);
    invalid["settings"]["apiSecretEnv"] = json!(local.api_secret_env);
    assert_eq!(apply_review(&http, &base, &invalid).await.status(), 400);
    // Full CallConfig, not the component DTO, rejects no transport and invalid TURN policy.
    let mut no_peer = local.clone();
    no_peer.peer.enabled = false;
    let no_peer = CallService::configured(pool.clone(), no_peer);
    let (no_peer_base, _no_peer_http) = component_test_http(&no_peer).await;
    assert_eq!(
        apply_review(&http, &no_peer_base, &body).await.status(),
        400
    );
    let mut invalid_turn = local.clone();
    invalid_turn.peer.allow_turn = true;
    let invalid_turn = CallService::configured(pool.clone(), invalid_turn);
    let (turn_base, _turn_http) = component_test_http(&invalid_turn).await;
    assert_eq!(apply_review(&http, &turn_base, &body).await.status(), 400);
    assert_eq!(service.component_view(true).await.unwrap(), applied["data"]);
    let request = CallInviteRequest {
        command_id: "managed-peer-only".into(),
        channel_id: 100,
        media_kind: CallMediaKind::Audio,
        invitee_ids: vec![2],
        supports_peer: true,
    };
    let created = service.invite(1, "peer-a", request.clone()).await.unwrap();
    assert_eq!(created["call"]["transport"], "webrtc");
    assert_eq!(service.invite(1, "peer-a", request).await.unwrap(), created);
    let id = created["call"]["call_id"].as_u64().unwrap();
    for (actor, device, command_id) in [(1, "peer-a", "join-a"), (2, "peer-b", "join-b")] {
        let joined = service
            .mutate(
                "join",
                actor,
                device,
                CallMutationRequest {
                    supports_peer: true,
                    ..mutation(command_id, id)
                },
            )
            .await
            .unwrap();
        assert_eq!(joined["url"], "");
        assert_eq!(joined["token"], "");
        assert!(
            joined["peer"]["ice_servers"].as_array().unwrap().is_empty(),
            "no TURN credentials or implicit relay"
        );
    }
    body["settings"]["enabled"] = json!(false);
    assert_eq!(
        apply_review(&http, &base, &body).await.status(),
        409,
        "peer calls also fence configuration updates"
    );
    pool.close().await;
}

#[tokio::test]
async fn privileged_config_recovers_missing_credentials_and_rejects_cross_role_env_reads() {
    let (_db, pool) = component_test_pool("components_recovery").await;
    let prefix = format!("RTC_RECOVERY_{}", uuid::Uuid::new_v4().simple());
    let key_ref = format!("{prefix}_KEY");
    let secret_ref = format!("{prefix}_SECRET");
    let unrelated_ref = format!("{prefix}_DATABASE_URL");
    let key_value = "review-only-livekit-key";
    let secret_value = "review-only-livekit-secret-not-an-issuer";
    let unrelated_value = "postgres://must-never-be-an-issuer";
    std::env::set_var(&key_ref, key_value);
    std::env::set_var(&secret_ref, secret_value);
    std::env::set_var(&unrelated_ref, unrelated_value);
    let local = CallConfig {
        api_key_env: key_ref.clone(),
        api_secret_env: secret_ref.clone(),
        ..Default::default()
    };
    let initial = CallService::configured(pool.clone(), local.clone());
    let old = initial.component_view(true).await.unwrap();
    let settings = json!({"enabled":true,"livekitUrl":"wss://rtc.example.invalid","apiKeyEnv":key_ref,"apiSecretEnv":secret_ref});
    let mut body = json!({"expectedRevision":old["revision"],"settings":settings});
    let (base, _server) = component_test_http(&initial).await;
    let http = reqwest::Client::new();
    for enabled in [true, false] {
        for (field, value) in [
            ("apiKeyEnv", secret_ref.as_str()),
            ("apiSecretEnv", key_ref.as_str()),
            ("apiKeyEnv", unrelated_ref.as_str()),
            ("apiSecretEnv", unrelated_ref.as_str()),
            ("apiKeyEnv", "DATABASE_URL"),
        ] {
            let mut bad = body.clone();
            bad["settings"]["enabled"] = json!(enabled);
            bad["settings"][field] = json!(value);
            let res = apply_review(&http, &base, &bad).await;
            assert_eq!(res.status(), 400);
            let error: Value = res.json().await.unwrap();
            for value in [key_value, secret_value, unrelated_value] {
                assert!(!error.to_string().contains(value));
            }
            assert_eq!(
                initial.component_view(true).await.unwrap(),
                old,
                "a rejected reference must not change revision/state"
            );
        }
    }
    let res = apply_review(&http, &base, &body).await;
    assert_eq!(res.status(), 200);
    let applied: Value = res.json().await.unwrap();
    assert_eq!(applied["data"]["runtimeAvailable"], true);
    std::env::remove_var(&key_ref);
    std::env::remove_var(&secret_ref);
    let restarted = CallService::configured(pool.clone(), local.clone());
    let (recovery_base, _recovery_server) = component_test_http(&restarted).await;
    let res = http
        .get(&recovery_base)
        .header("X-Service-Key", "component-review-key")
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let unavailable: Value = res.json().await.unwrap();
    assert_eq!(unavailable["data"]["revision"], applied["data"]["revision"]);
    assert_eq!(unavailable["data"]["settings"], settings);
    assert_eq!(unavailable["data"]["runtimeAvailable"], false);
    body["expectedRevision"] = unavailable["data"]["revision"].clone();
    // These are the same approved role names, not an allowlist-negative masquerading as missing env.
    let missing = apply_review(&http, &recovery_base, &body).await;
    assert_eq!(missing.status(), 400);
    assert_eq!(
        restarted.component_view(true).await.unwrap(),
        unavailable["data"]
    );
    for value in [key_value, secret_value, unrelated_value] {
        assert!(!unavailable.to_string().contains(value));
    }
    let router = privchat::rpc::router::RpcRouter::new();
    components::register(&router, Arc::new(restarted.clone())).await;
    let req = privchat::rpc::types::RPCMessageRequest {
        route: "config/get".into(),
        body: json!({"privileged":true}),
    };
    let public = router
        .handle(req.clone(), RpcContext::new().with_user_id("1".into()))
        .await;
    assert_eq!(
        public.code,
        privchat_protocol::ErrorCode::ServiceUnavailable.code() as i32
    );
    assert!(public.data.is_none());
    for service_key in [None, Some("ordinary-user-token")] {
        let mut request = http.get(&recovery_base).bearer_auth("ordinary-user-token");
        if let Some(key) = service_key {
            request = request.header("X-Service-Key", key);
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    body["expectedRevision"] = unavailable["data"]["revision"].clone();
    body["settings"] = json!({"enabled":false,"livekitUrl":"","apiKeyEnv":"","apiSecretEnv":""});
    let res = apply_review(&http, &recovery_base, &body).await;
    assert_eq!(res.status(), 200);
    let repaired: Value = res.json().await.unwrap();
    assert_eq!(repaired["data"]["runtimeAvailable"], true);
    let public = router
        .handle(req.clone(), RpcContext::new().with_user_id("1".into()))
        .await;
    assert_eq!(public.code, 0);
    let data = public.data.unwrap();
    assert_eq!(data["components"]["rtc"]["enabled"], false);
    assert!(data.get("runtimeAvailable").is_none());
    assert!(data.get("settings").is_none());
    // Restore allowed credentials, then prove persisted pre-allowlist rows cannot bypass the boundary.
    std::env::set_var(&key_ref, key_value);
    std::env::set_var(&secret_ref, secret_value);
    body["expectedRevision"] = repaired["data"]["revision"].clone();
    body["settings"] = settings.clone();
    let res = apply_review(&http, &recovery_base, &body).await;
    assert_eq!(res.status(), 200);
    let restored: Value = res.json().await.unwrap();
    let mut legacy = settings.clone();
    legacy["apiKeyEnv"] = json!(secret_ref);
    sqlx::query("UPDATE privchat_component_config SET settings=$1 WHERE component='rtc'")
        .bind(&legacy)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        restarted.runtime().await.is_err(),
        "a stored secret cannot be resolved as JWT iss"
    );
    let denied_join = restarted
        .mutate("join", 1, "review-device", mutation("legacy-env-issuer", 1))
        .await
        .unwrap_err();
    assert_eq!(
        denied_join.code,
        privchat_protocol::ErrorCode::ServiceUnavailable
    );
    assert!(!denied_join.message.contains(secret_value));
    assert!(
        components::get(&restarted, &RpcContext::new().with_user_id("1".into()))
            .await
            .is_err()
    );
    let view = restarted.component_view(true).await.unwrap();
    assert_eq!(view["runtimeAvailable"], false);
    assert_eq!(view["revision"], restored["data"]["revision"]);
    assert!(!view.to_string().contains(secret_value));
    body["expectedRevision"] = view["revision"].clone();
    body["settings"] = json!({"enabled":false,"livekitUrl":"","apiKeyEnv":"","apiSecretEnv":""});
    assert_eq!(
        apply_review(&http, &recovery_base, &body).await.status(),
        200
    );
    std::env::remove_var(&key_ref);
    std::env::remove_var(&secret_ref);
    std::env::remove_var(&unrelated_ref);
    pool.close().await;
}

#[tokio::test]
async fn unavailable_managed_runtime_still_expires_leases_without_base_sfu_cleanup() {
    let (_db, pool) = component_test_pool("component_expiry_recovery").await;
    sqlx::query("INSERT INTO privchat_users(user_id,username,qr_key) VALUES(1,'expiry1','expiry1'),(2,'expiry2','expiry2')").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO privchat_groups(group_id,name,owner_id,qr_key) VALUES(200,'expiry',1,'expiry')").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO privchat_channels(channel_id,channel_type,group_id) VALUES(200,1,200)",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO privchat_group_members(group_id,user_id,role) VALUES(200,1,0),(200,2,2)",
    )
    .execute(&pool)
    .await
    .unwrap();
    // Any accidental cleanup against the old local cluster is observable.
    let old_sfu = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let old_url = format!("ws://{}", old_sfu.local_addr().unwrap());
    let prefix = format!("COMPONENT_EXPIRY_{}", uuid::Uuid::new_v4().simple());
    let key_ref = format!("{prefix}_KEY");
    let secret_ref = format!("{prefix}_SECRET");
    std::env::set_var(&key_ref, "expiry-managed-key");
    std::env::set_var(&secret_ref, "expiry-managed-secret-not-production");
    let local = CallConfig {
        enabled: true,
        livekit_url: old_url,
        api_key_env: key_ref.clone(),
        api_secret_env: secret_ref.clone(),
        ..Default::default()
    };
    let service = CallService::new(
        pool.clone(),
        local.clone(),
        Some(("old-key".into(), "old-secret".into())),
    )
    .unwrap();
    let (base, _server) = component_test_http(&service).await;
    let http = reqwest::Client::new();
    let old = service.component_view(true).await.unwrap();
    let body = json!({"expectedRevision":old["revision"],"settings":{
        "enabled":true,"livekitUrl":"wss://managed-expiry.example.test",
        "apiKeyEnv":key_ref,"apiSecretEnv":secret_ref
    }});
    let applied = apply_review(&http, &base, &body).await;
    assert_eq!(applied.status(), 200);
    let applied: Value = applied.json().await.unwrap();
    let created = service
        .invite(1, "expiry-a", invite("expiry-create"))
        .await
        .unwrap();
    let id = created["call"]["call_id"].as_u64().unwrap();
    service
        .mutate("join", 2, "expiry-b", mutation("expiry-join", id))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, String>("SELECT state FROM privchat_calls WHERE call_id=$1")
            .bind(id as i64)
            .fetch_one(&pool)
            .await
            .unwrap(),
        "active"
    );
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(jsonb_set(document,'{claims,1,lease_until}','0'),'{claims,2,lease_until}','0') WHERE call_id=$1").bind(id as i64).execute(&pool).await.unwrap();
    std::env::remove_var(&key_ref);
    std::env::remove_var(&secret_ref);
    let restarted = CallService::configured(pool.clone(), local);
    assert!(restarted.runtime().await.is_err());
    assert!(restarted.component_view(false).await.is_err());
    assert!(restarted
        .invite(1, "expiry-a", invite("unavailable-new"))
        .await
        .is_err());
    assert!(restarted
        .mutate("join", 2, "expiry-b", mutation("unavailable-token", id))
        .await
        .is_err());
    assert!(
        !restarted.sweep_for_worker().await.unwrap(),
        "unavailable runtime must pause effect dispatch"
    );
    let document: Value =
        sqlx::query_scalar("SELECT document FROM privchat_calls WHERE call_id=$1")
            .bind(id as i64)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(document["call"]["state"], "ended");
    let snapshot: CallSnapshot = serde_json::from_value(document["call"].clone()).unwrap();
    assert!(restarted.media_cleanup(&snapshot, None).await.is_err());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), old_sfu.accept())
            .await
            .is_err(),
        "must never contact base SFU"
    );
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM privchat_call_effects WHERE NOT completed AND kind IN ('delete_room','remove_participant')").fetch_one(&pool).await.unwrap();
    assert!(
        pending > 0,
        "expiry must leave media cleanup durable, not pretend it succeeded"
    );
    let view = restarted.component_view(true).await.unwrap();
    assert_eq!(view["runtimeAvailable"], false);
    assert_eq!(view["revision"], applied["data"]["revision"]);
    let (recovery_base, _recovery_server) = component_test_http(&restarted).await;
    let disable = json!({"expectedRevision":view["revision"],"settings":{"enabled":false,"livekitUrl":"","apiKeyEnv":"","apiSecretEnv":""}});
    assert_eq!(
        apply_review(&http, &recovery_base, &disable).await.status(),
        409
    );
    assert!(!restarted.sweep_for_worker().await.unwrap());
    assert_eq!(restarted.component_view(true).await.unwrap(), view);
    pool.close().await;
}
