//! Real PostgreSQL migrations/outbox and actual loopback HTTP requests, not device delivery.
mod common;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    routing::post,
    Json, Router,
};
use privchat::service::{
    call_config::{CallConfig, CallPeerSettings, CallPushSettings},
    call_service::{CallHttpPushProvider, CallPushWorker, CallService},
};
use privchat_protocol::rpc::call::*;
use serde_json::{json, Value};
use sqlx::PgPool;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone)]
struct Request {
    path: String,
    headers: HeaderMap,
    body: Value,
}
#[derive(Default)]
struct Capture {
    requests: Vec<Request>,
    replies: VecDeque<(StatusCode, Value)>,
}
struct HttpFixture {
    capture: Arc<Mutex<Capture>>,
    provider: Arc<CallHttpPushProvider>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for HttpFixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl HttpFixture {
    async fn start() -> Self {
        async fn receive(
            State(capture): State<Arc<Mutex<Capture>>>,
            uri: Uri,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            let mut c = capture.lock().unwrap();
            c.requests.push(Request {
                path: uri.path().into(),
                headers,
                body,
            });
            let (status, body) = c
                .replies
                .pop_front()
                .unwrap_or((StatusCode::OK, json!({"name":"fixture/accepted"})));
            (status, Json(body))
        }
        let capture = Arc::new(Mutex::new(Capture::default()));
        let router = Router::new()
            .route("/3/device/{token}", post(receive))
            .route("/v1/projects/fixture/messages:send", post(receive))
            .with_state(capture.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            capture,
            provider: Arc::new(CallHttpPushProvider::loopback_fixture(&endpoint).unwrap()),
            task,
        }
    }
    fn requests(&self) -> Vec<Request> {
        self.capture.lock().unwrap().requests.clone()
    }
    fn reply(&self, status: StatusCode, body: Value) {
        self.capture
            .lock()
            .unwrap()
            .replies
            .push_back((status, body));
    }
}

struct Fixture {
    _db: common::ScratchDatabase,
    service: CallService,
    caller: Uuid,
    ios: Uuid,
    android: Uuid,
}
impl Fixture {
    async fn start() -> Self {
        let db = common::ScratchDatabase::create(&format!("call_push_{}", Uuid::new_v4().simple()))
            .await
            .expect("Real PostgreSQL required; this suite never skips");
        let mut conn = db.connect().await;
        privchat::migrate::apply_pending(&mut conn, |_| {})
            .await
            .unwrap();
        drop(conn);
        let pool = PgPool::connect(&db.url()).await.unwrap();
        for id in 1..=3i64 {
            sqlx::query("INSERT INTO privchat_users(user_id,username,qr_key) VALUES($1,$2,$2)")
                .bind(id)
                .bind(format!("callpush{id}"))
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO privchat_channels(channel_id,channel_type,direct_user1_id,direct_user2_id) VALUES(100,0,1,2)").execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO privchat_channel_participants(channel_id,user_id) VALUES(100,1),(100,2)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let caller = Uuid::new_v4();
        let ios = Uuid::new_v4();
        let android = Uuid::new_v4();
        for (user, device, kind) in [
            (1i64, caller, "ios"),
            (2, ios, "ios"),
            (2, android, "android"),
        ] {
            sqlx::query(
                "INSERT INTO privchat_devices(user_id,device_id,device_type) VALUES($1,$2,$3)",
            )
            .bind(user)
            .bind(device)
            .bind(kind)
            .execute(&pool)
            .await
            .unwrap();
        }
        let config = CallConfig {
            enabled: true,
            peer: CallPeerSettings {
                enabled: true,
                ..Default::default()
            },
            push: CallPushSettings {
                enabled: true,
                max_attempts: 3,
            },
            ..Default::default()
        };
        let service = CallService::with_peer_secret(pool, config, None, None).unwrap();
        Self {
            _db: db,
            service,
            caller,
            ios,
            android,
        }
    }
    async fn register(&self, user: u64, device: Uuid, platform: CallPushPlatform, token: &str) {
        assert!(
            self.service
                .register_push_device(
                    user,
                    &device.to_string(),
                    1,
                    CallPushRegisterRequest {
                        platform,
                        token: token.into()
                    }
                )
                .await
                .unwrap()
                .registered
        );
    }
    async fn invite(&self) -> u64 {
        let value = self
            .service
            .invite(
                1,
                &self.caller.to_string(),
                CallInviteRequest {
                    command_id: "invite-1".into(),
                    supports_peer: true,
                    channel_id: 100,
                    media_kind: CallMediaKind::Audio,
                    invitee_ids: vec![],
                },
            )
            .await
            .unwrap();
        assert_eq!(value["call"]["transport"], "webrtc");
        value["call"]["call_id"].as_u64().unwrap()
    }
    async fn mutate(&self, route: &str, user: u64, device: Uuid, id: u64) {
        self.service
            .mutate(
                route,
                user,
                &device.to_string(),
                CallMutationRequest {
                    command_id: format!("{route}-{user}"),
                    call_id: id,
                    supports_peer: true,
                },
            )
            .await
            .unwrap();
    }
    async fn count(&self, status: &str, kind: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT count(*) FROM privchat_call_push_outbox WHERE status=$1 AND kind=$2",
        )
        .bind(status)
        .bind(kind)
        .fetch_one(&self.service.pool)
        .await
        .unwrap()
    }
    async fn due(&self) {
        sqlx::query("UPDATE privchat_call_push_outbox SET next_attempt_at=0")
            .execute(&self.service.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn real_http_multidevice_answer_and_non_voip_cancel() {
    let f = Fixture::start().await;
    let http = HttpFixture::start().await;
    f.register(2, f.ios, CallPushPlatform::ApnsVoip, &"a1".repeat(32))
        .await;
    f.register(2, f.android, CallPushPlatform::Fcm, "android-fixture-token")
        .await;
    sqlx::query("INSERT INTO privchat_user_devices(user_id,device_id,platform,vendor,push_token,apns_armed) VALUES(2,$1,'ios','apns',$2,true)")
        .bind(f.ios.to_string()).bind("b2".repeat(32)).execute(&f.service.pool).await.unwrap();
    let call = f.invite().await;
    assert_eq!(
        f.invite().await,
        call,
        "Command replay must not duplicate deliveries"
    );
    assert_eq!(f.count("pending", "incoming").await, 2);
    let worker = CallPushWorker::new(f.service.clone(), http.provider.clone());
    assert!(worker.dispatch_one().await.unwrap());
    assert!(worker.dispatch_one().await.unwrap());
    assert!(!worker.dispatch_one().await.unwrap());
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    let apple = requests
        .iter()
        .find(|r| r.path.starts_with("/3/device/"))
        .unwrap();
    assert_eq!(apple.path, format!("/3/device/{}", "a1".repeat(32)));
    assert_eq!(apple.headers["apns-push-type"], "voip");
    assert_eq!(apple.headers["apns-topic"], "dev.privchat.fixture.voip");
    assert_eq!(apple.headers["apns-priority"], "10");
    assert_eq!(apple.headers["apns-expiration"], "0");
    let data = &apple.body["data"];
    assert!(data.as_object().unwrap().values().all(Value::is_string));
    assert_eq!(data["call_id"], call.to_string());
    assert_eq!(data["recipient_user_id"], "2");
    assert_eq!(data["type"], "call.incoming");
    assert_eq!(data["schema_version"], "1");
    assert_eq!(
        data["call_uuid"],
        Uuid::new_v5(
            &Uuid::NAMESPACE_URL,
            format!("privchat:call:{call}").as_bytes()
        )
        .to_string()
    );
    assert_eq!(
        data.as_object().unwrap().len(),
        11,
        "No media credentials or unversioned fields"
    );
    let google = requests
        .iter()
        .find(|r| r.path.ends_with("messages:send"))
        .unwrap();
    assert_eq!(google.body["message"]["android"]["priority"], "HIGH");
    assert!(google.body["message"]["notification"].is_null());
    assert_eq!(google.body["message"]["data"], *data);
    f.mutate("join", 2, f.android, call).await;
    assert_eq!(
        f.count("pending", "cancel").await,
        1,
        "Do not cancel the accepting device"
    );
    assert!(worker.dispatch_one().await.unwrap());
    let cancel = http.requests().pop().unwrap();
    assert_eq!(cancel.path, format!("/3/device/{}", "b2".repeat(32)));
    assert_eq!(cancel.headers["apns-push-type"], "background");
    assert_eq!(cancel.headers["apns-topic"], "dev.privchat.fixture");
    assert_eq!(cancel.headers["apns-priority"], "5");
    assert_eq!(cancel.body["data"]["type"], "call.cancel");
    f.mutate("end", 1, f.caller, call).await;
    assert!(worker.dispatch_one().await.unwrap());
    let end = http.requests().pop().unwrap();
    assert_eq!(end.body["message"]["data"]["type"], "call.cancel");
    assert_eq!(end.body["message"]["android"]["ttl"], "0s");
    assert!(!worker.dispatch_one().await.unwrap());
}

#[tokio::test]
async fn real_http_retry_survives_worker_restart_and_exhausts() {
    let f = Fixture::start().await;
    let http = HttpFixture::start().await;
    f.register(2, f.ios, CallPushPlatform::ApnsVoip, &"a1".repeat(32))
        .await;
    f.invite().await;
    http.reply(StatusCode::SERVICE_UNAVAILABLE, json!({}));
    let worker = CallPushWorker::new(f.service.clone(), http.provider.clone());
    assert!(worker.dispatch_one().await.unwrap());
    assert_eq!(f.count("pending", "incoming").await, 1);
    assert!(
        !worker.dispatch_one().await.unwrap(),
        "Retry must respect backoff"
    );
    f.due().await;
    let restarted = CallPushWorker::new(f.service.clone(), http.provider.clone());
    assert!(restarted.dispatch_one().await.unwrap());
    assert_eq!(f.count("sent", "incoming").await, 1);
    let requests = http.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[0].headers["apns-id"],
        requests[1].headers["apns-id"]
    );
    f.register(2, f.android, CallPushPlatform::Fcm, "late-android-token")
        .await;
    for _ in 0..3 {
        http.reply(StatusCode::TOO_MANY_REQUESTS, json!({}));
        f.due().await;
        assert!(restarted.dispatch_one().await.unwrap());
    }
    assert_eq!(f.count("failed", "incoming").await, 1);
    assert!(!restarted.dispatch_one().await.unwrap());
    let attempts: i32 =
        sqlx::query_scalar("SELECT attempts FROM privchat_call_push_outbox WHERE device_id=$1")
            .bind(f.android)
            .fetch_one(&f.service.pool)
            .await
            .unwrap();
    assert_eq!(attempts, 3);
}

#[tokio::test]
async fn ownership_revocation_expiry_and_invalid_token_fail_closed() {
    let f = Fixture::start().await;
    let http = HttpFixture::start().await;
    for (user, device, platform) in [
        (3, f.ios, CallPushPlatform::ApnsVoip),
        (2, Uuid::new_v4(), CallPushPlatform::ApnsVoip),
        (2, f.android, CallPushPlatform::ApnsVoip),
    ] {
        assert!(f
            .service
            .register_push_device(
                user,
                &device.to_string(),
                1,
                CallPushRegisterRequest {
                    platform,
                    token: "a1".repeat(32)
                }
            )
            .await
            .is_err());
    }
    for extra in [
        "user_id",
        "device_id",
        "bundle_id",
        "environment",
        "endpoint",
    ] {
        let mut body = json!({"platform":"apns_voip","token":"aa"});
        body[extra] = json!("untrusted");
        assert!(serde_json::from_value::<CallPushRegisterRequest>(body).is_err());
    }
    assert!(CallHttpPushProvider::from_config(&Default::default()).is_err());
    assert!(CallHttpPushProvider::loopback_fixture("https://example.org").is_err());
    f.register(2, f.ios, CallPushPlatform::ApnsVoip, &"A1".repeat(32))
        .await;
    f.register(2, f.android, CallPushPlatform::Fcm, "android-revoked")
        .await;
    let call = f.invite().await;
    sqlx::query("UPDATE privchat_devices SET session_version=session_version+1 WHERE user_id=2 AND device_id=$1")
        .bind(f.android).execute(&f.service.pool).await.unwrap();
    assert!(
        f.service
            .register_push_device(
                2,
                &f.android.to_string(),
                1,
                CallPushRegisterRequest {
                    platform: CallPushPlatform::Fcm,
                    token: "stale-session-token".into()
                }
            )
            .await
            .is_err(),
        "Old authenticated sessions cannot re-arm revoked tokens"
    );
    http.reply(StatusCode::BAD_REQUEST, json!({"reason":"BadDeviceToken"}));
    let worker = CallPushWorker::new(f.service.clone(), http.provider.clone());
    assert!(worker.dispatch_one().await.unwrap());
    assert!(worker.dispatch_one().await.unwrap());
    assert_eq!(
        http.requests().len(),
        1,
        "Revoked sessions must not reach provider"
    );
    let registrations: i64 =
        sqlx::query_scalar("SELECT count(*) FROM privchat_call_push_devices WHERE device_id=$1")
            .bind(f.ios)
            .fetch_one(&f.service.pool)
            .await
            .unwrap();
    assert_eq!(registrations, 0, "Explicit invalid token must be removed");
    f.register(2, f.ios, CallPushPlatform::ApnsVoip, &"c3".repeat(32))
        .await;
    sqlx::query("UPDATE privchat_call_push_outbox SET expires_at=0 WHERE kind='incoming'")
        .execute(&f.service.pool)
        .await
        .unwrap();
    assert!(worker.dispatch_one().await.unwrap());
    assert_eq!(
        http.requests().len(),
        1,
        "Expired invites must not reach provider"
    );
    assert_eq!(f.count("expired", "incoming").await, 1);
    f.mutate("cancel", 1, f.caller, call).await;
    while worker.dispatch_one().await.unwrap() {}
    assert_eq!(
        http.requests().len(),
        1,
        "No ordinary APNs token: never substitute VoIP cancellation"
    );
    assert_eq!(f.count("unavailable", "cancel").await, 1);
    assert!(
        !f.service
            .unregister_push_device(2, &f.ios.to_string())
            .await
            .unwrap()
            .registered
    );
    assert!(
        !f.service
            .unregister_push_device(2, &f.ios.to_string())
            .await
            .unwrap()
            .registered
    );
}

#[tokio::test]
async fn answer_before_delivery_suppresses_all_pending_invites() {
    let f = Fixture::start().await;
    let http = HttpFixture::start().await;
    f.register(2, f.ios, CallPushPlatform::ApnsVoip, &"a1".repeat(32))
        .await;
    f.register(2, f.android, CallPushPlatform::Fcm, "android-answer")
        .await;
    let call = f.invite().await;
    f.mutate("join", 2, f.android, call).await;
    assert_eq!(f.count("pending", "incoming").await, 0);
    assert_eq!(f.count("cancelled", "incoming").await, 2);
    let worker = CallPushWorker::new(f.service.clone(), http.provider.clone());
    while worker.dispatch_one().await.unwrap() {}
    assert!(
        http.requests().is_empty(),
        "Losing iOS device has no ordinary control token, never send VoIP"
    );
    assert_eq!(f.count("unavailable", "cancel").await, 1);
}
