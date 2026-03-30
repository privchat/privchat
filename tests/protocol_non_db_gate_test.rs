use msgtrans::SessionId;
use privchat::context::{ErrorResponseBuilder, RequestContext};
use privchat::infra::auth_whitelist::{
    is_anonymous_message_type, is_anonymous_rpc_route, list_anonymous_message_types,
    list_anonymous_rpc_routes,
};
use privchat_protocol::protocol::{MessageType, PingRequest, RpcRequest};
use privchat_protocol::{decode_message, encode_message};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[test]
fn anonymous_whitelist_is_protocol_safe() {
    assert!(is_anonymous_message_type(&MessageType::AuthorizationRequest));
    assert!(!is_anonymous_message_type(&MessageType::SendMessageRequest));

    assert!(is_anonymous_rpc_route("system/health"));
    assert!(is_anonymous_rpc_route("system/info"));
    assert!(!is_anonymous_rpc_route("message/send"));

    let message_types = list_anonymous_message_types();
    assert!(message_types.contains(&MessageType::AuthorizationRequest));

    let routes = list_anonymous_rpc_routes();
    assert!(routes.contains(&"system/health".to_string()));
    assert!(routes.contains(&"system/info".to_string()));
}

#[test]
fn protocol_encode_decode_roundtrip_without_db() {
    let ping = PingRequest { timestamp: 123_456 };
    let encoded = encode_message(&ping).expect("encode ping");
    let decoded: PingRequest = decode_message(&encoded).expect("decode ping");
    assert_eq!(decoded.timestamp, ping.timestamp);

    let rpc = RpcRequest {
        route: "system/health".to_string(),
        body: serde_json::json!({"probe": "ready"}),
    };
    let encoded = encode_message(&rpc).expect("encode rpc");
    let decoded: RpcRequest = decode_message(&encoded).expect("decode rpc");
    assert_eq!(decoded.route, "system/health");
    assert_eq!(decoded.body["probe"], "ready");
}

#[test]
fn request_context_json_and_auth_status_work() {
    let session_id: SessionId = 7u64.into();
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9527);
    let ctx = RequestContext::new(session_id, br#"{"k":"v"}"#.to_vec(), addr);

    assert!(!ctx.is_authenticated());
    assert_eq!(ctx.data_as_json().expect("json")["k"], "v");

    let authed = ctx.clone().with_user_id("1001".to_string());
    assert!(authed.is_authenticated());
}

#[test]
fn error_response_builder_produces_json() {
    let session_id: SessionId = 9u64.into();
    let bytes = ErrorResponseBuilder::protocol_error(session_id, "bad packet");
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("error json");
    assert_eq!(value["type"], "error");
    assert_eq!(value["error"]["code"], "PROTOCOL_ERROR");
    assert_eq!(value["error"]["message"], "bad packet");
}
