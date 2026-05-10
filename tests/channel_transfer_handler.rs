// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Integration tests for `privchat::handler::channel_transfer_handler`
// (Channel Transfer Bite 2: wire ingress).
//
// Tests target the public `process_transfer_request` free function so we
// don't need to construct a real msgtrans `SessionId` / `RequestContext`.
// The relay client is exercised against a tiny axum loopback server, same
// pattern as `tests/transfer.rs` (Bite 1).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use tokio::sync::Mutex;

use privchat::channel_transfer::{
    ChannelTransferRelayClient, ForwardTransferRequest, ForwardTransferResponse,
    SERVICE_KEY_HEADER, MAX_TRANSFER_BODY_BYTES,
};
use privchat::config::ChannelTransferConfig;
use privchat::handler::channel_transfer_handler::{
    process_transfer_request, ChannelTransferLookups, CODE_AUTH_REQUIRED,
    CODE_CHANNEL_NOT_SUBSCRIBED, CODE_INVALID_PARAMS, CODE_PAYLOAD_TOO_LARGE, CODE_TIMEOUT,
};

// =====================================================================
// Helpers — wire encode + canned axum mock app + canned lookups
// =====================================================================

fn encode_wire_request(
    request_id: &str,
    channel_id: u64,
    route: &str,
    body: &[u8],
) -> Vec<u8> {
    let req = privchat_protocol::TransferRequest {
        request_id: request_id.to_string(),
        channel_id,
        route: route.to_string(),
        body: body.to_vec(),
    };
    privchat_protocol::encode_message(&req).expect("encode wire TransferRequest")
}

fn decode_wire_response(bytes: &[u8]) -> privchat_protocol::TransferResponse {
    privchat_protocol::decode_message::<privchat_protocol::TransferResponse>(bytes)
        .expect("decode wire TransferResponse")
}

/// Lookups that return canned values without standing up real infra.
struct CannedLookups {
    is_subscribed: bool,
    resolve_room: Option<u64>,
    is_room_member: bool,
}

impl CannedLookups {
    fn happy_path() -> Self {
        Self {
            is_subscribed: true,
            resolve_room: Some(90001),
            is_room_member: true,
        }
    }

    fn not_subscribed() -> Self {
        Self {
            is_subscribed: false,
            ..Self::happy_path()
        }
    }
}

#[async_trait]
impl ChannelTransferLookups for CannedLookups {
    async fn is_subscribed(&self, _user_id: u64, _channel_id: u64) -> bool {
        self.is_subscribed
    }
    async fn resolve_room(&self, _channel_id: u64) -> Option<u64> {
        self.resolve_room
    }
    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        self.is_room_member
    }
}

#[derive(Default)]
struct Capture {
    service_key: Option<String>,
    request: Option<ForwardTransferRequest>,
}

/// Spawn a mock application server that captures the inbound request and
/// replies with the canned response. Returns the bound address.
async fn spawn_mock_app(
    capture: Arc<Mutex<Capture>>,
    canned: ForwardTransferResponse,
) -> SocketAddr {
    let canned = Arc::new(canned);
    let app = Router::new()
        .route(
            "/service/privchat/transfer/dispatch",
            post(
                move |State((cap, canned)): State<(
                    Arc<Mutex<Capture>>,
                    Arc<ForwardTransferResponse>,
                )>,
                      headers: HeaderMap,
                      Json(req): Json<ForwardTransferRequest>| async move {
                    let mut g = cap.lock().await;
                    g.service_key = headers
                        .get(SERVICE_KEY_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .map(|s| s.to_string());
                    g.request = Some(req);
                    Json((*canned).clone())
                },
            ),
        )
        .with_state((capture, canned));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::task::yield_now().await;
    addr
}

/// Mock app that intentionally never responds — used to drive the timeout
/// path. Sleeps for an absurd duration before returning.
async fn spawn_silent_app() -> SocketAddr {
    let app = Router::new().route(
        "/service/privchat/transfer/dispatch",
        post(|_headers: HeaderMap, _body: String| async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            (StatusCode::OK, "")
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::task::yield_now().await;
    addr
}

fn relay_client_for(addr: SocketAddr, timeout_ms: u64) -> ChannelTransferRelayClient {
    let cfg = ChannelTransferConfig {
        application_url: format!("http://{addr}"),
        application_master_key: "test-master-key".to_string(),
        timeout_ms,
    };
    ChannelTransferRelayClient::new(&cfg).expect("relay client builds")
}

const VALID_REQ_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const VALID_CHANNEL: u64 = 2817;
const VALID_USER: u64 = 100002077;

// =====================================================================
// Tests
// =====================================================================

#[tokio::test]
async fn unauthenticated_returns_auth_required() {
    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", b"x");
    // Pin a relay that's never reached — auth check must short-circuit before relay.
    let addr = spawn_silent_app().await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path();

    let bytes = process_transfer_request(&wire, None, &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_AUTH_REQUIRED);
    assert_eq!(resp.request_id, VALID_REQ_ID);
    assert_eq!(resp.channel_id, VALID_CHANNEL);
}

#[tokio::test]
async fn invalid_request_id_returns_invalid_params() {
    // request_id "42" is too short.
    let wire = encode_wire_request("42", VALID_CHANNEL, "game/poker/raise", b"x");
    let addr = spawn_silent_app().await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path();

    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_INVALID_PARAMS);
    assert_eq!(resp.request_id, "42");
    assert_eq!(resp.channel_id, VALID_CHANNEL);
}

#[tokio::test]
async fn slash_route_returns_invalid_params() {
    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "/game/poker/raise", b"x");
    let addr = spawn_silent_app().await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path();

    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_INVALID_PARAMS);
    assert!(
        resp.message.contains("/"),
        "expected leading-slash detail in message, got {:?}",
        resp.message
    );
}

#[tokio::test]
async fn body_over_64kb_returns_payload_too_large() {
    let big = vec![0u8; MAX_TRANSFER_BODY_BYTES + 1];
    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", &big);
    let addr = spawn_silent_app().await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path();

    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn unsubscribed_returns_channel_not_subscribed() {
    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", b"x");
    let addr = spawn_silent_app().await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::not_subscribed();

    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_CHANNEL_NOT_SUBSCRIBED);
    assert_eq!(resp.request_id, VALID_REQ_ID);
}

#[tokio::test]
async fn application_timeout_returns_timeout() {
    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", b"x");
    let addr = spawn_silent_app().await;
    // Aggressive 200ms timeout so the test runs fast.
    let relay = relay_client_for(addr, 200);
    let lookups = CannedLookups::happy_path();

    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");
    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, CODE_TIMEOUT);
    assert_eq!(resp.request_id, VALID_REQ_ID);
    assert_eq!(resp.channel_id, VALID_CHANNEL);
}

#[tokio::test]
async fn success_response_keeps_original_client_request_id() {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let canned = ForwardTransferResponse {
        client_request_id: VALID_REQ_ID.to_string(),
        channel_id: VALID_CHANNEL,
        code: 0,
        message: "OK".to_string(),
        data: Some(b"raise-result".to_vec()),
    };
    let addr = spawn_mock_app(capture.clone(), canned).await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path();

    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", b"raise-200");
    let bytes = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");

    let resp = decode_wire_response(&bytes);
    assert_eq!(resp.code, 0);
    assert_eq!(resp.message, "OK");
    assert_eq!(
        resp.request_id, VALID_REQ_ID,
        "wire response.request_id MUST equal original client_request_id (spec §5.1)"
    );
    assert_eq!(resp.channel_id, VALID_CHANNEL);
    assert_eq!(resp.data.as_deref(), Some(b"raise-result".as_ref()));
}

#[tokio::test]
async fn forward_request_carries_user_id_room_id_internal_request_id_and_trace_id() {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let canned = ForwardTransferResponse {
        client_request_id: VALID_REQ_ID.to_string(),
        channel_id: VALID_CHANNEL,
        code: 0,
        message: "OK".to_string(),
        data: None,
    };
    let addr = spawn_mock_app(capture.clone(), canned).await;
    let relay = relay_client_for(addr, 3_000);
    let lookups = CannedLookups::happy_path(); // resolve_room → 90001

    let wire = encode_wire_request(VALID_REQ_ID, VALID_CHANNEL, "game/poker/raise", b"hi");
    let _ = process_transfer_request(&wire, Some(VALID_USER), &lookups, &relay)
        .await
        .expect("encode response");

    let cap = capture.lock().await;
    let received = cap.request.as_ref().expect("relay reached the mock app");

    // Wire-driven fields preserved
    assert_eq!(received.client_request_id, VALID_REQ_ID);
    assert_eq!(received.channel_id, VALID_CHANNEL);
    assert_eq!(received.route, "game/poker/raise");
    assert_eq!(received.body, b"hi");

    // Server-injected fields
    assert_eq!(received.user_id, VALID_USER);
    assert_eq!(received.room_id, 90001, "server must inject room_id from lookups");
    assert!(
        !received.internal_request_id.is_empty(),
        "server must generate internal_request_id"
    );
    assert_ne!(
        received.internal_request_id, received.client_request_id,
        "internal_request_id MUST be distinct from client_request_id (spec §7.3 naming)"
    );
    assert!(
        !received.trace_id.is_empty(),
        "server must generate trace_id"
    );

    // X-Service-Key header was on the wire
    assert_eq!(cap.service_key.as_deref(), Some("test-master-key"));
}
