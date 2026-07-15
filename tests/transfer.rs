// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Integration tests for `privchat::channel_transfer` validators.
//
// The outbound HTTP relay client that used to live here moved to
// `server_event` (SERVER_EVENT_DISPATCH_SPEC v1.1 unified outbound); the
// relay/Forward* test half was removed with it — server_event has no test
// coverage yet (tracked on the remediation board).

use privchat::channel_transfer::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
    ChannelTransferValidationError, MAX_TRANSFER_BODY_BYTES, MAX_TRANSFER_REQUEST_ID_LEN,
};

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
