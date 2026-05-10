// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Integration tests for `privchat::channel_transfer` (Channel Transfer
// Bite 1: validators + outbound HTTP relay client).
//
// Lives in `tests/` rather than inline `#[cfg(test)]` so it bypasses
// pre-existing test-mode compile breakage in unrelated `infra::session_manager`
// and `middleware::auth_middleware` modules.

use privchat::channel_transfer::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
    ChannelTransferRelayClient, ChannelTransferRelayError, ChannelTransferValidationError,
    ForwardTransferRequest, ForwardTransferResponse, MAX_TRANSFER_BODY_BYTES,
    MAX_TRANSFER_REQUEST_ID_LEN, SERVICE_KEY_HEADER,
};
use privchat::config::ChannelTransferConfig;

// =====================================================================
// validate_transfer_request_id
// =====================================================================

#[test]
fn request_id_uuid_v4_accepted() {
    assert!(
        validate_transfer_request_id("550e8400-e29b-41d4-a716-446655440000").is_ok(),
        "canonical UUID v4 should be accepted"
    );
}

#[test]
fn request_id_alnum_16_or_more_accepted() {
    // 26-char ULID-ish — comfortably ≥16 alnum bytes.
    assert!(validate_transfer_request_id("01HX9KY6X8R7G8A6R7Q9V3BTZP").is_ok());
    // Exactly 16 alnum bytes is the boundary — must pass.
    assert!(validate_transfer_request_id("aaaaaaaaaaaaaaaa").is_ok());
}

#[test]
fn request_id_short_rejected() {
    // 15 alnum bytes → rejected for low entropy.
    assert_eq!(
        validate_transfer_request_id("aaaaaaaaaaaaaaa"),
        Err(ChannelTransferValidationError::RequestIdLowEntropy)
    );
    assert_eq!(
        validate_transfer_request_id("42"),
        Err(ChannelTransferValidationError::RequestIdLowEntropy)
    );
    assert_eq!(
        validate_transfer_request_id("req_42"),
        Err(ChannelTransferValidationError::RequestIdLowEntropy)
    );
}

#[test]
fn request_id_empty_rejected() {
    assert_eq!(
        validate_transfer_request_id(""),
        Err(ChannelTransferValidationError::RequestIdEmpty)
    );
}

#[test]
fn request_id_too_long_rejected() {
    let s = "a".repeat(MAX_TRANSFER_REQUEST_ID_LEN + 1);
    assert!(matches!(
        validate_transfer_request_id(&s),
        Err(ChannelTransferValidationError::RequestIdTooLong { .. })
    ));
}

#[test]
fn request_id_bad_char_rejected() {
    // Spaces and slashes are not URL/log safe.
    assert_eq!(
        validate_transfer_request_id("not safe id 1234567890"),
        Err(ChannelTransferValidationError::RequestIdBadChar)
    );
    assert_eq!(
        validate_transfer_request_id("path/like/id/123456"),
        Err(ChannelTransferValidationError::RequestIdBadChar)
    );
}

// =====================================================================
// validate_transfer_route
// =====================================================================

#[test]
fn route_canonical_accepted() {
    assert!(validate_transfer_route("game/poker/raise").is_ok());
    assert!(validate_transfer_route("wallet/balance/query").is_ok());
    assert!(validate_transfer_route("bot/task/start").is_ok());
    assert!(validate_transfer_route("game/poker/private-cards").is_ok());
}

#[test]
fn route_leading_slash_rejected() {
    assert_eq!(
        validate_transfer_route("/game/poker/raise"),
        Err(ChannelTransferValidationError::RouteLeadingSlash)
    );
}

#[test]
fn route_wrong_segment_count_rejected() {
    assert_eq!(
        validate_transfer_route("game/poker"),
        Err(ChannelTransferValidationError::RouteShape)
    );
    assert_eq!(
        validate_transfer_route("game/poker/raise/extra"),
        Err(ChannelTransferValidationError::RouteShape)
    );
}

#[test]
fn route_uppercase_rejected() {
    assert_eq!(
        validate_transfer_route("Game/Poker/Raise"),
        Err(ChannelTransferValidationError::RouteSegmentBadChar)
    );
}

#[test]
fn route_underscore_or_dot_rejected() {
    assert_eq!(
        validate_transfer_route("game/poker/raise_now"),
        Err(ChannelTransferValidationError::RouteSegmentBadChar)
    );
    assert_eq!(
        validate_transfer_route("game/poker/raise.now"),
        Err(ChannelTransferValidationError::RouteSegmentBadChar)
    );
}

#[test]
fn route_empty_segment_rejected() {
    assert_eq!(
        validate_transfer_route("game//raise"),
        Err(ChannelTransferValidationError::RouteEmptySegment)
    );
}

#[test]
fn route_dash_at_segment_boundary_rejected() {
    assert_eq!(
        validate_transfer_route("game/-poker/raise"),
        Err(ChannelTransferValidationError::RouteSegmentBadChar)
    );
    assert_eq!(
        validate_transfer_route("game/poker-/raise"),
        Err(ChannelTransferValidationError::RouteSegmentBadChar)
    );
}

#[test]
fn route_too_long_rejected() {
    let mid = "a".repeat(300);
    let route = format!("svc/{mid}/act");
    assert!(matches!(
        validate_transfer_route(&route),
        Err(ChannelTransferValidationError::RouteTooLong { .. })
    ));
}

// =====================================================================
// validate_transfer_body_size
// =====================================================================

#[test]
fn body_within_limit_accepted() {
    assert!(validate_transfer_body_size(&[]).is_ok());
    assert!(validate_transfer_body_size(&vec![0u8; 1024]).is_ok());
    assert!(validate_transfer_body_size(&vec![0u8; MAX_TRANSFER_BODY_BYTES]).is_ok());
}

#[test]
fn body_over_limit_rejected() {
    let body = vec![0u8; MAX_TRANSFER_BODY_BYTES + 1];
    let err = validate_transfer_body_size(&body).unwrap_err();
    match err {
        ChannelTransferValidationError::BodyTooLarge { len, limit } => {
            assert_eq!(len, MAX_TRANSFER_BODY_BYTES + 1);
            assert_eq!(limit, MAX_TRANSFER_BODY_BYTES);
        }
        other => panic!("expected BodyTooLarge, got {other:?}"),
    }
}

// =====================================================================
// ForwardTransferRequest serialization
// =====================================================================

#[test]
fn forward_request_serializes_with_base64_body() {
    let req = ForwardTransferRequest {
        internal_request_id: "srv_req_001".to_string(),
        client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        channel_id: 2817,
        room_id: 90001,
        user_id: 100002077,
        route: "game/poker/raise".to_string(),
        body: b"hello".to_vec(),
        trace_id: "trace_001".to_string(),
    };
    let json = serde_json::to_value(&req).unwrap();
    assert_eq!(json["internal_request_id"], "srv_req_001");
    assert_eq!(
        json["client_request_id"],
        "550e8400-e29b-41d4-a716-446655440000"
    );
    assert_eq!(json["channel_id"], 2817);
    assert_eq!(json["room_id"], 90001);
    assert_eq!(json["user_id"], 100002077);
    assert_eq!(json["route"], "game/poker/raise");
    // base64("hello") == "aGVsbG8="
    assert_eq!(json["body"], "aGVsbG8=");
    assert_eq!(json["trace_id"], "trace_001");
}

#[test]
fn forward_response_deserializes_base64_data() {
    // base64("world") = "d29ybGQ="
    let payload = serde_json::json!({
        "client_request_id": "550e8400-e29b-41d4-a716-446655440000",
        "channel_id": 2817,
        "code": 0,
        "message": "OK",
        "data": "d29ybGQ="
    });
    let resp: ForwardTransferResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(resp.client_request_id, "550e8400-e29b-41d4-a716-446655440000");
    assert_eq!(resp.channel_id, 2817);
    assert_eq!(resp.code, 0);
    assert_eq!(resp.message, "OK");
    assert_eq!(resp.data, Some(b"world".to_vec()));
}

#[test]
fn forward_response_empty_data_is_none() {
    let payload = serde_json::json!({
        "client_request_id": "id_aaaaaaaaaaaaaaaa",
        "channel_id": 1,
        "code": 0,
        "message": "OK",
        "data": ""
    });
    let resp: ForwardTransferResponse = serde_json::from_value(payload).unwrap();
    assert!(resp.data.is_none());
}

#[test]
fn forward_response_missing_data_is_none() {
    let payload = serde_json::json!({
        "client_request_id": "id_aaaaaaaaaaaaaaaa",
        "channel_id": 1,
        "code": 20902,
        "message": "service not found"
    });
    let resp: ForwardTransferResponse = serde_json::from_value(payload).unwrap();
    assert_eq!(resp.code, 20902);
    assert!(resp.data.is_none());
}

// =====================================================================
// ChannelTransferRelayClient: real HTTP via tiny axum server
// =====================================================================

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Default)]
struct Capture {
    service_key: Option<String>,
    request: Option<ForwardTransferRequest>,
}

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

#[tokio::test]
async fn relay_sends_x_service_key_and_maps_response() {
    let capture: Arc<Mutex<Capture>> = Arc::new(Mutex::new(Capture::default()));
    let canned = ForwardTransferResponse {
        client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        channel_id: 2817,
        code: 0,
        message: "OK".to_string(),
        data: Some(b"raise-result".to_vec()),
    };
    let addr = spawn_mock_app(capture.clone(), canned).await;

    let cfg = ChannelTransferConfig {
        application_url: format!("http://{addr}"),
        application_master_key: "test-master-key".to_string(),
        timeout_ms: 3000,
    };
    let client = ChannelTransferRelayClient::new(&cfg).expect("relay client builds");
    assert!(
        client.endpoint().ends_with("/service/privchat/transfer/dispatch"),
        "endpoint must include relay path, got {}",
        client.endpoint()
    );

    let req = ForwardTransferRequest {
        internal_request_id: "srv_req_001".to_string(),
        client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        channel_id: 2817,
        room_id: 90001,
        user_id: 100002077,
        route: "game/poker/raise".to_string(),
        body: b"raise-200".to_vec(),
        trace_id: "trace_001".to_string(),
    };
    let resp = client.forward(&req).await.expect("relay succeeds");
    assert_eq!(resp.code, 0);
    assert_eq!(resp.message, "OK");
    assert_eq!(resp.data.as_deref(), Some(b"raise-result".as_ref()));
    assert_eq!(
        resp.client_request_id,
        "550e8400-e29b-41d4-a716-446655440000",
        "response must echo client_request_id verbatim — server uses it for the wire TransferResponse"
    );

    let cap = capture.lock().await;
    assert_eq!(cap.service_key.as_deref(), Some("test-master-key"));
    let received = cap.request.as_ref().expect("server got the request");
    assert_eq!(received.client_request_id, req.client_request_id);
    assert_eq!(received.body, b"raise-200");
    assert_eq!(received.route, "game/poker/raise");
}

#[tokio::test]
async fn relay_maps_app_http_error() {
    let app = Router::new().route(
        "/service/privchat/transfer/dispatch",
        post(|| async {
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "client_request_id": "550e8400-e29b-41d4-a716-446655440000",
                    "channel_id": 2817,
                    "code": 4,
                    "message": "boom",
                    "data": ""
                })),
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    tokio::task::yield_now().await;

    let cfg = ChannelTransferConfig {
        application_url: format!("http://{addr}"),
        application_master_key: "k".to_string(),
        timeout_ms: 3000,
    };
    let client = ChannelTransferRelayClient::new(&cfg).unwrap();
    let req = ForwardTransferRequest {
        internal_request_id: "i".to_string(),
        client_request_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
        channel_id: 2817,
        room_id: 0,
        user_id: 0,
        route: "game/poker/raise".to_string(),
        body: vec![],
        trace_id: "t".to_string(),
    };
    let err = client.forward(&req).await.expect_err("expected error");
    match err {
        ChannelTransferRelayError::AppHttpError { status, body, .. } => {
            assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
            let body = body.expect("error body parsed");
            assert_eq!(body.code, 4);
            assert_eq!(body.message, "boom");
        }
        other => panic!("expected AppHttpError, got {other:?}"),
    }
}
