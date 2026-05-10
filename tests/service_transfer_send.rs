// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Integration tests for `privchat::http::routes::transfer`
// (Channel Transfer Bite 3: POST /api/service/transfer/send).
//
// Tests target the public `process_app_transfer_send` free function so we
// don't need to spin up a full axum router or AdminServerState. Auth
// (X-Service-Key) is exercised by the existing `verify_service_key`
// middleware and out of scope here — same as how the other /api/service/*
// handlers are unit-tested.

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use tokio::sync::Mutex;

use privchat::channel_transfer::MAX_TRANSFER_BODY_BYTES;
use privchat::http::envelope::ApiEnvelope;
use privchat::http::routes::transfer::{
    process_app_transfer_send, TransferSendBackend, TransferSendOk, TransferSendOutcome,
    TransferSendRequest,
};

// =====================================================================
// Mock backend: canned answers + capture for the wire packet.
// =====================================================================

#[derive(Default)]
struct DeliverCapture {
    target_user_id: Option<u64>,
    channel_id: Option<u64>,
    encoded_packet: Option<Vec<u8>>,
}

struct CannedBackend {
    resolve_room: Option<u64>,
    is_user_online: bool,
    is_user_subscribed: bool,
    is_room_member: bool,
    /// What `deliver` should return.
    canned_delivered: usize,
    capture: Mutex<DeliverCapture>,
}

impl CannedBackend {
    fn happy_path() -> Self {
        Self {
            resolve_room: Some(90001),
            is_user_online: true,
            is_user_subscribed: true,
            is_room_member: true,
            canned_delivered: 1,
            capture: Mutex::new(DeliverCapture::default()),
        }
    }
}

#[async_trait]
impl TransferSendBackend for CannedBackend {
    async fn resolve_room(&self, _channel_id: u64) -> Option<u64> {
        self.resolve_room
    }
    async fn is_user_online(&self, _user_id: u64) -> bool {
        self.is_user_online
    }
    async fn is_user_subscribed(&self, _user_id: u64, _channel_id: u64) -> bool {
        self.is_user_subscribed
    }
    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        self.is_room_member
    }
    async fn deliver(
        &self,
        target_user_id: u64,
        channel_id: u64,
        encoded_packet: &[u8],
    ) -> usize {
        let mut g = self.capture.lock().await;
        g.target_user_id = Some(target_user_id);
        g.channel_id = Some(channel_id);
        g.encoded_packet = Some(encoded_packet.to_vec());
        self.canned_delivered
    }
}

// =====================================================================
// Builders
// =====================================================================

const VALID_REQ_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const VALID_CHANNEL: u64 = 2817;
const VALID_TARGET: u64 = 100002077;

fn req(body: &[u8]) -> TransferSendRequest {
    TransferSendRequest {
        request_id: VALID_REQ_ID.to_string(),
        channel_id: VALID_CHANNEL,
        target_user_id: VALID_TARGET,
        route: "game/poker/private-cards".to_string(),
        body: BASE64_STD.encode(body),
    }
}

fn req_with_route(route: &str) -> TransferSendRequest {
    let mut r = req(b"x");
    r.route = route.to_string();
    r
}

fn req_with_request_id(rid: &str) -> TransferSendRequest {
    let mut r = req(b"x");
    r.request_id = rid.to_string();
    r
}

fn req_raw_body(body_string: &str) -> TransferSendRequest {
    let mut r = req(b"x");
    r.body = body_string.to_string();
    r
}

fn assert_err(outcome: &TransferSendOutcome, expected_code: u32) {
    match outcome {
        TransferSendOutcome::Err { code, .. } => assert_eq!(*code, expected_code),
        TransferSendOutcome::Ok(ok) => panic!("expected Err({expected_code}), got Ok({ok:?})"),
    }
}

// =====================================================================
// Tests
// =====================================================================

#[tokio::test]
async fn rejects_invalid_request_id() {
    let backend = CannedBackend::happy_path();
    let outcome = process_app_transfer_send(req_with_request_id("42"), &backend).await;
    assert_err(&outcome, 10100);
}

#[tokio::test]
async fn rejects_slash_prefixed_route() {
    let backend = CannedBackend::happy_path();
    let outcome =
        process_app_transfer_send(req_with_route("/game/poker/private-cards"), &backend).await;
    assert_err(&outcome, 10100);
}

#[tokio::test]
async fn rejects_invalid_base64_body() {
    let backend = CannedBackend::happy_path();
    // '@' is not valid in standard base64 alphabet.
    let outcome = process_app_transfer_send(req_raw_body("not@valid@base64"), &backend).await;
    assert_err(&outcome, 10100);
    if let TransferSendOutcome::Err { message, .. } = outcome {
        assert!(
            message.contains("base64"),
            "expected base64 detail in message, got {message:?}"
        );
    }
}

#[tokio::test]
async fn rejects_body_over_64kb() {
    let backend = CannedBackend::happy_path();
    let big = vec![0u8; MAX_TRANSFER_BODY_BYTES + 1];
    let outcome = process_app_transfer_send(req(&big), &backend).await;
    assert_err(&outcome, 10106);
}

#[tokio::test]
async fn rejects_unsubscribed_target_when_online() {
    let mut backend = CannedBackend::happy_path();
    backend.is_user_subscribed = false;
    backend.is_user_online = true;
    let outcome = process_app_transfer_send(req(b"x"), &backend).await;
    assert_err(&outcome, 20900);
}

#[tokio::test]
async fn rejects_target_not_in_room() {
    let mut backend = CannedBackend::happy_path();
    backend.is_room_member = false;
    let outcome = process_app_transfer_send(req(b"x"), &backend).await;
    assert_err(&outcome, 10004);
}

#[tokio::test]
async fn returns_delivered_zero_when_target_offline() {
    let mut backend = CannedBackend::happy_path();
    // Offline → not subscribed and not online.
    backend.is_user_subscribed = false;
    backend.is_user_online = false;
    let outcome = process_app_transfer_send(req(b"x"), &backend).await;
    match outcome {
        TransferSendOutcome::Ok(TransferSendOk {
            accepted,
            delivered_sessions,
        }) => {
            assert!(accepted, "offline target is still 'accepted' best-effort");
            assert_eq!(delivered_sessions, 0);
        }
        other => panic!("expected Ok with 0 delivered, got {other:?}"),
    }
}

#[tokio::test]
async fn delivers_transfer_request_packet_to_target_sessions() {
    let backend = CannedBackend::happy_path(); // canned_delivered = 1
    let body = b"private-cards-payload";
    let outcome = process_app_transfer_send(req(body), &backend).await;

    match outcome {
        TransferSendOutcome::Ok(ok) => {
            assert!(ok.accepted);
            assert_eq!(ok.delivered_sessions, 1);
        }
        other => panic!("expected Ok, got {other:?}"),
    }

    // Verify the wire packet shape captured by the mock backend.
    let cap = backend.capture.lock().await;
    assert_eq!(cap.target_user_id, Some(VALID_TARGET));
    assert_eq!(cap.channel_id, Some(VALID_CHANNEL));
    let raw = cap
        .encoded_packet
        .as_ref()
        .expect("delivery received an encoded packet");
    let decoded: privchat_protocol::TransferRequest = privchat_protocol::decode_message(raw)
        .expect("encoded payload must round-trip as TransferRequest");
    assert_eq!(decoded.request_id, VALID_REQ_ID);
    assert_eq!(decoded.channel_id, VALID_CHANNEL);
    assert_eq!(decoded.route, "game/poker/private-cards");
    assert_eq!(decoded.body, body);
    // Schema-level enforcement: TransferRequest has no `target_user_id` field.
    // Asserted indirectly by the type system (decoded is `TransferRequest`).
}

#[tokio::test]
async fn success_response_uses_api_envelope_shape() {
    let backend = CannedBackend::happy_path();
    let outcome = process_app_transfer_send(req(b"x"), &backend).await;

    let ok = match outcome {
        TransferSendOutcome::Ok(ok) => ok,
        other => panic!("expected Ok, got {other:?}"),
    };
    let envelope = ApiEnvelope::ok(ok);
    let json = serde_json::to_value(&envelope).expect("envelope serializes to JSON");

    // Top-level: code / message / data only.
    assert_eq!(json["code"], 0);
    assert_eq!(json["message"], "OK");
    let data = &json["data"];
    assert!(data.is_object(), "data must be an object, got {data}");
    assert_eq!(data["accepted"], true);
    assert_eq!(data["delivered_sessions"], 1);

    // Top-level must NOT carry the business fields directly (envelope spec).
    assert!(json.get("accepted").is_none());
    assert!(json.get("delivered_sessions").is_none());

    // No extra top-level keys beyond {code, message, data}.
    let obj = json.as_object().unwrap();
    let keys: std::collections::BTreeSet<_> = obj.keys().map(|k| k.as_str()).collect();
    assert_eq!(
        keys,
        ["code", "data", "message"]
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>(),
        "envelope must have exactly {{code, message, data}}, got {keys:?}"
    );
}

#[tokio::test]
async fn error_response_uses_api_envelope_with_data_null() {
    let mut backend = CannedBackend::happy_path();
    backend.is_user_subscribed = false;
    backend.is_user_online = true;
    let outcome = process_app_transfer_send(req(b"x"), &backend).await;

    let (code, message, http_status) = match outcome {
        TransferSendOutcome::Err {
            code,
            message,
            http_status,
        } => (code, message, http_status),
        other => panic!("expected Err, got {other:?}"),
    };
    assert_eq!(code, 20900);
    assert_eq!(http_status, StatusCode::FORBIDDEN);

    let envelope = ApiEnvelope::err_raw(code, message);
    let json = serde_json::to_value(&envelope).expect("envelope serializes to JSON");
    assert_eq!(json["code"], 20900);
    assert!(json["message"].is_string());
    assert!(json["data"].is_null(), "errors must have data: null");
}

// Wrapping the `Arc<dyn TransferSendBackend>` path also works — sanity check
// for the production composition where the backend is shared.
#[tokio::test]
async fn arc_dyn_backend_is_supported() {
    let backend: Arc<dyn TransferSendBackend> = Arc::new(CannedBackend::happy_path());
    let outcome = process_app_transfer_send(req(b"x"), backend.as_ref()).await;
    assert!(matches!(outcome, TransferSendOutcome::Ok(_)));
}
