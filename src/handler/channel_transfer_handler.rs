// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `MessageType::TransferRequest` (biz_type 19) wire ingress.
//!
//! Per server spec §1.4 this handler is **not** a business service: it does
//! auth / format / channel access / room-membership gating, then forwards the
//! request to the application via the **generic** [`ServerEventClient`]
//! (envelope `event_type = "transfer.requested"`) and turns the application's
//! `TransferResponsePayload` back into a wire `TransferResponse` (biz_type 20).
//!
//! v1.0 起 server↔app 边界不再单开 `/transfer/dispatch`；transfer 走统一的
//! `/service/privchat/server-event/dispatch`。详见
//! `SERVER_EVENT_DISPATCH_SPEC` § "request-response event types"。
//!
//! 本 handler **MUST NOT** 引入：`service_id` / business registry / `callback_url` /
//! `route` prefix vs `service.name` 校验 / application 幂等 / 审计 / 业务分发——
//! 这些全部由 `neton-application-module-privchat` 承担。

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use privchat_protocol::protocol::MessageType;
use tracing::info;

use crate::channel_transfer::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
};
use crate::context::RequestContext;
use crate::dispatcher::MessageDispatcher;
use crate::error::{Result, ServerError};
use crate::handler::MessageHandler;
use crate::infra::{ConnectionManager, SubscribeManager};
use crate::server_event::types::TransferResponsePayload;
use crate::server_event::types::TransferCaller;
use crate::server_event::{ServerEvent, ServerEventClient, ServerEventError};

// =====================================================================
// Wire codes — subset of `protocol::ErrorCode` actually used by the
// server-side Channel Transfer handlers (spec §15.1).
// =====================================================================

/// 0 — `Ok`. Used only when the application's response was a success and we
/// are echoing back its fields.
pub const CODE_OK: i32 = 0;
/// 3 — `ServiceUnavailable`. The application HTTP call failed at the
/// transport layer (connect refused, DNS, etc.).
pub const CODE_SERVICE_UNAVAILABLE: i32 = 3;
/// 4 — `InternalError`. Encoding/decoding bug or unclassified relay error.
pub const CODE_INTERNAL_ERROR: i32 = 4;
/// 5 — `Timeout`. The application did not respond within the configured
/// `timeout_ms`. server returns this immediately; the application may still
/// finish executing.
pub const CODE_TIMEOUT: i32 = 5;
/// 10000 — `AuthRequired`. The session is not authenticated.
pub const CODE_AUTH_REQUIRED: i32 = 10000;
/// 10004 — `PermissionDenied`. Authenticated but not a member of the room
/// behind `channel_id`.
pub const CODE_PERMISSION_DENIED: i32 = 10004;
/// 10100 — `InvalidParams`. `request_id` / `route` failed format validation.
pub const CODE_INVALID_PARAMS: i32 = 10100;
/// 10106 — `PayloadTooLarge`. `body` exceeded the 64 KB cap.
pub const CODE_PAYLOAD_TOO_LARGE: i32 = 10106;
/// 20500 — `ChannelNotFound`. `channel_id` did not resolve to a known room.
pub const CODE_CHANNEL_NOT_FOUND: i32 = 20500;
/// `ChannelNotSubscribed` (Channel Transfer segment, 21500). Reserved for
/// service-handler-level use; wire ingress generally does not return it.
///
/// Sourced from `privchat_protocol::ErrorCode` rather than a local literal:
/// the previous value 20900 collides with `SyncChannelResyncRequired`, which
/// the SDK resume state machine branches on. See ERROR_CODE_SPEC §3.4.12.
pub const CODE_CHANNEL_NOT_SUBSCRIBED: i32 =
    privchat_protocol::ErrorCode::ChannelNotSubscribed as i32;

// =====================================================================
// Lookups — abstracted so tests can mock without standing up
// ChannelService / ConnectionManager
// =====================================================================

#[async_trait]
pub trait ChannelTransferLookups: Send + Sync {
    async fn is_subscribed(&self, user_id: u64, channel_id: u64) -> bool;
    async fn resolve_room(&self, channel_id: u64) -> Option<u64>;
    async fn is_room_member(&self, user_id: u64, room_id: u64) -> bool;
}

pub struct DefaultChannelTransferLookups {
    connection_manager: Arc<ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
}

impl DefaultChannelTransferLookups {
    pub fn new(
        connection_manager: Arc<ConnectionManager>,
        subscribe_manager: Arc<SubscribeManager>,
    ) -> Self {
        Self {
            connection_manager,
            subscribe_manager,
        }
    }
}

#[async_trait]
impl ChannelTransferLookups for DefaultChannelTransferLookups {
    async fn is_subscribed(&self, user_id: u64, channel_id: u64) -> bool {
        let conns = self.connection_manager.get_user_connections(user_id).await;
        for c in conns {
            let channels = self.subscribe_manager.get_session_channels(&c.session_id);
            if channels.contains(&channel_id) {
                return true;
            }
        }
        false
    }

    async fn resolve_room(&self, channel_id: u64) -> Option<u64> {
        Some(channel_id)
    }

    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        true
    }
}

// =====================================================================
// Handler
// =====================================================================

pub struct ChannelTransferHandler {
    lookups: Arc<dyn ChannelTransferLookups>,
    server_event_client: Arc<ServerEventClient>,
    session_manager: Arc<crate::infra::SessionManager>,
}

impl ChannelTransferHandler {
    pub fn new(
        lookups: Arc<dyn ChannelTransferLookups>,
        server_event_client: Arc<ServerEventClient>,
        session_manager: Arc<crate::infra::SessionManager>,
    ) -> Self {
        Self {
            lookups,
            server_event_client,
            session_manager,
        }
    }
}

/// 从认证会话取发起方身份。会话不存在或与 ctx 的 user 不一致时返回 None（按未认证拒绝）。
pub fn caller_from_session(
    ctx_user_id: Option<&str>,
    session: Option<&crate::infra::session_manager::SessionInfo>,
) -> Option<TransferCaller> {
    let session = session?;
    let user_id = ctx_user_id?.parse::<u64>().ok()?;
    if session.user_id.parse::<u64>().ok()? != user_id {
        return None;
    }
    Some(TransferCaller {
        user_id,
        device_id: session.device_id.clone(),
        session_version: session.jwt_claims.session_version,
        scope: session.jwt_claims.scope.clone(),
    })
}

#[async_trait]
impl MessageHandler for ChannelTransferHandler {
    async fn handle(&self, ctx: RequestContext) -> Result<Option<Vec<u8>>> {
        let session = self.session_manager.get_session_info(&ctx.session_id).await;
        let caller = caller_from_session(ctx.user_id.as_deref(), session.as_ref());
        let bytes = process_transfer_request(
            &ctx.data,
            caller,
            self.lookups.as_ref(),
            self.server_event_client.as_ref(),
        )
        .await?;
        Ok(Some(bytes))
    }

    fn name(&self) -> &'static str {
        "ChannelTransferHandler"
    }
}

// =====================================================================
// Pure ingress pipeline (testable without RequestContext / SessionId)
// =====================================================================

/// Decode + validate + emit ServerEvent + encode wire response.
///
/// 实现说明：
///
/// 1. wire `TransferRequest` decode + 校验（auth / request_id / route / body size /
///    channel access / room membership）
/// 2. 包装成 `ServerEvent::transfer_requested(...)`（spec `SERVER_EVENT_DISPATCH_SPEC`
///    §4 + `transfer.requested` event_type）；`internal_event_id` 由 envelope
///    builder 自动生成 UUID，**不**等于 client 的 `request_id`
/// 3. `ServerEventClient.send(event)` 发到下游统一入口
///    `/service/privchat/server-event/dispatch`
/// 4. 解析下游 ack：
///    - 顶层 `accepted=true, code=0` + `response_payload` 非空 → 解码为
///      `TransferResponsePayload`，把业务 `code` / `message` / `data` 编进 wire
///      `TransferResponse`
///    - 顶层 `code != 0` → 把"投递/调度失败"映射为 wire 错误码（如 handler 未注册）
///    - 传输级失败（超时 / 连接拒绝 / 解析失败）→ 映射为 wire 错误码
pub async fn process_transfer_request(
    raw_wire: &[u8],
    caller: Option<TransferCaller>,
    lookups: &dyn ChannelTransferLookups,
    server_event_client: &ServerEventClient,
) -> Result<Vec<u8>> {
    // 1. Decode wire TransferRequest
    let req: privchat_protocol::TransferRequest = match privchat_protocol::decode_message(raw_wire)
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("ChannelTransferHandler: wire decode failed: {e}");
            return encode_response("", 0, CODE_INVALID_PARAMS, "decode failed", &[]);
        }
    };

    // 2. Auth
    let Some(caller) = caller else {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_AUTH_REQUIRED,
            "auth required",
            &[],
        );
    };

    // 3. Format gates (server spec §6 / §7 / §10)
    if let Err(e) = validate_transfer_request_id(&req.request_id) {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_INVALID_PARAMS,
            &e.to_string(),
            &[],
        );
    }
    if let Err(e) = validate_transfer_route(&req.route) {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_INVALID_PARAMS,
            &e.to_string(),
            &[],
        );
    }
    if let Err(e) = validate_transfer_body_size(&req.body) {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_PAYLOAD_TOO_LARGE,
            &e.to_string(),
            &[],
        );
    }

    // 4. Subscription — **不**校验（spec CHANNEL_TRANSFER_SPEC §1.5）

    // 5. channel_id → room_id
    let Some(room_id) = lookups.resolve_room(req.channel_id).await else {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_CHANNEL_NOT_FOUND,
            "channel not found",
            &[],
        );
    };

    // 6. Room membership
    if !lookups.is_room_member(caller.user_id, room_id).await {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_PERMISSION_DENIED,
            "room membership required",
            &[],
        );
    }

    // 7. 包装成 ServerEvent(event_type=transfer.requested) 投递给下游
    let occurred_at_ms = chrono::Utc::now().timestamp_millis();
    let event = match ServerEvent::transfer_requested(
        req.request_id.clone(),
        &caller,
        req.channel_id,
        room_id,
        req.route.clone(),
        req.body.clone(),
        occurred_at_ms,
    ) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("ChannelTransferHandler: build ServerEvent failed: {e}");
            return encode_response(
                &req.request_id,
                req.channel_id,
                CODE_INTERNAL_ERROR,
                "internal error",
                &[],
            );
        }
    };

    match server_event_client.send(&event).await {
        Ok(ack) => {
            // ack.code != 0 表示下游投递/调度失败（如 handler 未注册）；
            // 这种情况 response_payload 为空，把顶层 code 直接当 wire 错误回。
            if ack.code != 0 {
                tracing::warn!(
                    "ChannelTransferHandler: downstream non-zero envelope code: code={} message={}",
                    ack.code,
                    ack.message
                );
                return encode_response(
                    &req.request_id,
                    req.channel_id,
                    CODE_SERVICE_UNAVAILABLE,
                    &ack.message,
                    &[],
                );
            }

            // accepted=true, code=0 → 解码 response_payload 拿 transfer 业务结果
            let payload_b64 = match ack.response_payload {
                Some(s) if !s.is_empty() => s,
                _ => {
                    tracing::warn!(
                        "ChannelTransferHandler: downstream OK but response_payload missing"
                    );
                    return encode_response(
                        &req.request_id,
                        req.channel_id,
                        CODE_INTERNAL_ERROR,
                        "response_payload missing",
                        &[],
                    );
                }
            };
            let payload_bytes = match BASE64_STD.decode(&payload_b64) {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(
                        "ChannelTransferHandler: response_payload base64 decode failed: {e}"
                    );
                    return encode_response(
                        &req.request_id,
                        req.channel_id,
                        CODE_INTERNAL_ERROR,
                        "response_payload decode failed",
                        &[],
                    );
                }
            };
            let transfer_resp: TransferResponsePayload =
                match serde_json::from_slice(&payload_bytes) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(
                            "ChannelTransferHandler: response_payload JSON parse failed: {e}"
                        );
                        return encode_response(
                            &req.request_id,
                            req.channel_id,
                            CODE_INTERNAL_ERROR,
                            "response_payload parse failed",
                            &[],
                        );
                    }
                };

            // request_id / channel_id 防御性兜底：downstream 通常按原值透传，
            // 偶尔会漏（如它构造默认值），fall back 到请求里的原值。
            let resp_request_id = if transfer_resp.request_id.is_empty() {
                req.request_id.clone()
            } else {
                transfer_resp.request_id
            };
            let resp_channel_id = if transfer_resp.channel_id == 0 {
                req.channel_id
            } else {
                transfer_resp.channel_id
            };
            let resp_data_bytes = if transfer_resp.data.is_empty() {
                Vec::new()
            } else {
                BASE64_STD.decode(&transfer_resp.data).unwrap_or_else(|e| {
                    tracing::warn!(
                        "ChannelTransferHandler: transfer_resp.data base64 decode failed: {e}; treating as empty"
                    );
                    Vec::new()
                })
            };
            encode_response(
                &resp_request_id,
                resp_channel_id,
                transfer_resp.code,
                &transfer_resp.message,
                &resp_data_bytes,
            )
        }
        Err(ServerEventError::Timeout) => encode_response(
            &req.request_id,
            req.channel_id,
            CODE_TIMEOUT,
            "application timeout",
            &[],
        ),
        Err(ServerEventError::Transport(msg)) => {
            tracing::warn!("ChannelTransferHandler: server-event transport error: {msg}");
            encode_response(
                &req.request_id,
                req.channel_id,
                CODE_SERVICE_UNAVAILABLE,
                "application unavailable",
                &[],
            )
        }
        Err(other) => {
            tracing::warn!("ChannelTransferHandler: server-event relay error: {other}");
            encode_response(
                &req.request_id,
                req.channel_id,
                CODE_INTERNAL_ERROR,
                "internal error",
                &[],
            )
        }
    }
}

fn encode_response(
    request_id: &str,
    channel_id: u64,
    code: i32,
    message: &str,
    data: &[u8],
) -> Result<Vec<u8>> {
    let resp = privchat_protocol::TransferResponse {
        request_id: request_id.to_string(),
        channel_id,
        code,
        message: message.to_string(),
        data: if data.is_empty() {
            None
        } else {
            Some(data.to_vec())
        },
    };
    privchat_protocol::encode_message(&resp)
        .map_err(|e| ServerError::Internal(format!("encode TransferResponse: {e}")).into())
}

// =====================================================================
// Registration entry point — gated on `server_event_client` presence
// =====================================================================

/// Register [`ChannelTransferHandler`] against `MessageType::TransferRequest`
/// iff a [`ServerEventClient`] is available (i.e. `[server_event]` configured).
///
/// 没有 `[server_event]` 时跳过注册——client 发 wire `TransferRequest` 会拿到
/// "unsupported message type"。这是预期行为：transfer wire 入站必须有下游可投递。
///
/// Returns `Ok(true)` if registered, `Ok(false)` if skipped.
pub fn try_register_channel_transfer_handler(
    dispatcher: &mut MessageDispatcher,
    server_event_client: Option<Arc<ServerEventClient>>,
    connection_manager: Arc<ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
    session_manager: Arc<crate::infra::SessionManager>,
) -> Result<bool> {
    let Some(client) = server_event_client else {
        info!(
            "📡 Channel Transfer: 未启用（缺 [server_event] 配置 → 无下游可投递），\
             跳过 wire ingress 注册"
        );
        return Ok(false);
    };

    let lookups: Arc<dyn ChannelTransferLookups> = Arc::new(DefaultChannelTransferLookups::new(
        connection_manager,
        subscribe_manager,
    ));
    dispatcher.register_handler(
        MessageType::TransferRequest,
        Box::new(ChannelTransferHandler::new(lookups, client.clone(), session_manager)),
    );
    info!(
        "✅ Channel Transfer: wire ingress 已注册（出站 → {} via ServerEvent transfer.requested）",
        client.endpoint()
    );
    Ok(true)
}

#[cfg(test)]
mod caller_identity_tests {
    use super::*;
    use crate::auth::token_service::UnifiedTokenClaims;
    use crate::infra::session_manager::SessionInfo;

    fn session(user_id: &str, device_id: &str, version: i64, scope: &[&str]) -> SessionInfo {
        SessionInfo {
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
            authenticated_at: chrono::Utc::now(),
            last_active_at: chrono::Utc::now(),
            jwt_claims: UnifiedTokenClaims {
                iss: "privchat-server".into(),
                sub: user_id.to_string(),
                aud: vec![],
                iat: 0,
                exp: 0,
                jti: "j".into(),
                token_type: "access".into(),
                device_id: device_id.to_string(),
                session_version: version,
                scope: scope.iter().map(|s| s.to_string()).collect(),
                business_system_id: "privchat-application".into(),
                app_id: "weey-agent".into(),
            },
            client_pts: 0,
            ready_for_push: true,
        }
    }

    #[test]
    fn caller_comes_from_authenticated_session() {
        let s = session("1001", "node-a", 7, &["agent_node"]);
        let caller = caller_from_session(Some("1001"), Some(&s)).expect("caller");
        assert_eq!(
            caller,
            TransferCaller {
                user_id: 1001,
                device_id: "node-a".into(),
                session_version: 7,
                scope: vec!["agent_node".into()],
            }
        );
    }

    #[test]
    fn missing_or_mismatched_session_is_unauthenticated() {
        let s = session("1001", "node-a", 7, &["agent_node"]);
        assert!(caller_from_session(Some("1001"), None).is_none());
        assert!(caller_from_session(None, Some(&s)).is_none());
        assert!(caller_from_session(Some("1002"), Some(&s)).is_none());
    }

    /// 上行事件里的设备身份只来自会话：wire 包里没有、也无法放入 device_id。
    #[test]
    fn transfer_event_carries_session_device_identity() {
        let caller = TransferCaller {
            user_id: 1001,
            device_id: "node-a".into(),
            session_version: 7,
            scope: vec!["agent_node".into()],
        };
        let event = ServerEvent::transfer_requested(
            "550e8400-e29b-41d4-a716-446655440000".into(),
            &caller,
            42,
            42,
            "agent/node/report".into(),
            br#"{"device_id":"node-b","user_id":9}"#.to_vec(),
            0,
        )
        .expect("event");
        let raw = BASE64_STD.decode(&event.payload).expect("base64 payload");
        let payload: serde_json::Value = serde_json::from_slice(&raw).expect("json payload");
        assert_eq!(payload["user_id"], 1001);
        assert_eq!(payload["device_id"], "node-a");
        assert_eq!(payload["session_version"], 7);
        assert_eq!(payload["scope"], serde_json::json!(["agent_node"]));
    }
}
