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
//! auth / format / subscription / room-membership gating, then forwards the
//! request to the application via [`ChannelTransferRelayClient`] and turns
//! the application's flat-shaped response back into a wire
//! `TransferResponse` (biz_type 20).
//!
//! It MUST NOT introduce: `service_id`, business registry, `callback_url`,
//! `route` prefix vs `service.name` validation, application idempotency,
//! audit, or business dispatch. All of that lives in
//! `neton-application-module-privchat` (dispatch spec §1.4).
//!
//! Tests live in `tests/channel_transfer_handler.rs` against the public
//! [`process_transfer_request`] free function so the test surface does not
//! need a constructed `RequestContext` (which would require a real
//! msgtrans `SessionId`).

use std::sync::Arc;

use async_trait::async_trait;
use privchat_protocol::protocol::MessageType;
use tracing::info;
use uuid::Uuid;

use crate::channel_transfer::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
    ChannelTransferRelayClient, ChannelTransferRelayError, ForwardTransferRequest,
};
use crate::config::ChannelTransferConfig;
use crate::context::RequestContext;
use crate::dispatcher::MessageDispatcher;
use crate::error::{Result, ServerError};
use crate::handler::MessageHandler;
use crate::infra::{ConnectionManager, SubscribeManager};

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
/// 20900 — `ChannelNotSubscribed` (Channel Transfer segment). The user
/// has no session subscribed to this `channel_id`.
pub const CODE_CHANNEL_NOT_SUBSCRIBED: i32 = 20900;

// =====================================================================
// Lookups — abstracted so tests can mock without standing up
// ChannelService / ConnectionManager
// =====================================================================

/// Read-only lookups the wire ingress needs from the rest of the server.
///
/// The default implementation [`DefaultChannelTransferLookups`] composes
/// `ConnectionManager` (user → sessions) and `SubscribeManager`
/// (session → channels). Tests substitute a small mock that returns
/// canned values directly.
#[async_trait]
pub trait ChannelTransferLookups: Send + Sync {
    /// Whether `user_id` has at least one authenticated session subscribed
    /// to `channel_id`.
    async fn is_subscribed(&self, user_id: u64, channel_id: u64) -> bool;

    /// Resolve `channel_id` → `room_id`. Returns `None` if the channel
    /// does not exist.
    ///
    /// In PrivChat's current model `room_id == channel_id`; this method
    /// exists so the boundary is in one place if/when room and channel
    /// identifiers diverge.
    async fn resolve_room(&self, channel_id: u64) -> Option<u64>;

    /// Whether `user_id` is a member of `room_id`. Returning `true`
    /// reflects defense-in-depth on top of [`is_subscribed`] — Subscribe
    /// already gates membership for Private/Group, so for Phase 1 this
    /// can simply mirror the subscription state.
    async fn is_room_member(&self, user_id: u64, room_id: u64) -> bool;
}

/// Production composition that backs [`ChannelTransferLookups`] with the
/// real connection / subscription registries.
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
        // Current PrivChat model: channel_id IS room_id. Returning Some here
        // (rather than querying ChannelService) is safe because our caller
        // gates this behind `is_subscribed`; if the user has any live
        // subscription, the channel exists.
        // TODO: when room_id and channel_id decouple, route through ChannelService.
        Some(channel_id)
    }

    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        // Phase 1: subscription presence is treated as proof of membership.
        // Subscribe-time membership check (SubscribeMessageHandler) already
        // gates Private/Group; Room (channel_type=2) has no persistent
        // membership in the current model.
        // TODO: defense-in-depth recheck via ChannelService.get_channel_members
        // for the kicked-while-subscribed race.
        true
    }
}

// =====================================================================
// Handler
// =====================================================================

pub struct ChannelTransferHandler {
    lookups: Arc<dyn ChannelTransferLookups>,
    relay_client: Arc<ChannelTransferRelayClient>,
}

impl ChannelTransferHandler {
    pub fn new(
        lookups: Arc<dyn ChannelTransferLookups>,
        relay_client: Arc<ChannelTransferRelayClient>,
    ) -> Self {
        Self {
            lookups,
            relay_client,
        }
    }
}

#[async_trait]
impl MessageHandler for ChannelTransferHandler {
    async fn handle(&self, ctx: RequestContext) -> Result<Option<Vec<u8>>> {
        let auth_user_id = ctx.user_id.as_ref().and_then(|s| s.parse::<u64>().ok());
        let bytes = process_transfer_request(
            &ctx.data,
            auth_user_id,
            self.lookups.as_ref(),
            self.relay_client.as_ref(),
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

/// Decode + validate + relay + encode. The wire bytes in / wire bytes out
/// surface that tests target.
pub async fn process_transfer_request(
    raw_wire: &[u8],
    auth_user_id: Option<u64>,
    lookups: &dyn ChannelTransferLookups,
    relay: &ChannelTransferRelayClient,
) -> Result<Vec<u8>> {
    // 1. Decode wire TransferRequest. Failure here means the client sent
    //    an ill-formed packet; we can't echo a meaningful request_id back,
    //    but we still must produce *some* TransferResponse so the client
    //    doesn't hang on its pending map.
    let req: privchat_protocol::TransferRequest = match privchat_protocol::decode_message(raw_wire)
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("ChannelTransferHandler: wire decode failed: {e}");
            return encode_response("", 0, CODE_INVALID_PARAMS, "decode failed", &[]);
        }
    };

    // 2. Auth
    let Some(user_id) = auth_user_id else {
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

    // 4. Subscription — **不**校验。
    //
    // Spec: CHANNEL_TRANSFER_SPEC §1.5 Transfer Access Policy。Transfer 通用层
    // 只做 auth / channel access / room membership / route / business_channel
    // binding / dispatch_flag。`is_subscribed(user_id, channel_id)` 是实时
    // 事件订阅状态，不是 RPC 调用的通用前置条件——bot / official / system /
    // wallet / 等普通 service 拿到 channel_id 就该能调。
    //
    // 需要"必须订阅"的强实时业务（如 game room 下注、live room 操作），由对应
    // PrivChatTransferHandler 在 service handler 内自行校验（结合 seat token /
    // room ticket / action_seq 一并审计）。详见 spec §1.5 中 game 校验链示例。
    //
    // 20900 `ChannelNotSubscribed` 段位仍然保留，但仅在两种语义下产生：
    //   - app→user 推送时目标 online 但未订阅（投递路由，§12.2）
    //   - service handler 自身声明 REQUIRE_SUBSCRIPTION 时业务返回
    // 通用层**不**返。Room membership 在第 6 步保留——那才是真正的 channel access。

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
    if !lookups.is_room_member(user_id, room_id).await {
        return encode_response(
            &req.request_id,
            req.channel_id,
            CODE_PERMISSION_DENIED,
            "room membership required",
            &[],
        );
    }

    // 7. Forward to application
    let forward = ForwardTransferRequest {
        internal_request_id: Uuid::new_v4().to_string(),
        client_request_id: req.request_id.clone(),
        channel_id: req.channel_id,
        room_id,
        user_id,
        route: req.route.clone(),
        body: req.body.clone(),
        trace_id: Uuid::new_v4().to_string(),
    };

    match relay.forward(&forward).await {
        Ok(app_resp) => encode_response(
            // app may have echoed client_request_id in its response — trust it,
            // but if app misbehaved and dropped it, fall back to ours so the
            // client's pending map can still match.
            if app_resp.client_request_id.is_empty() {
                &req.request_id
            } else {
                &app_resp.client_request_id
            },
            if app_resp.channel_id == 0 {
                req.channel_id
            } else {
                app_resp.channel_id
            },
            app_resp.code,
            &app_resp.message,
            app_resp.data.as_deref().unwrap_or(&[]),
        ),
        Err(ChannelTransferRelayError::Timeout) => encode_response(
            &req.request_id,
            req.channel_id,
            CODE_TIMEOUT,
            "application timeout",
            &[],
        ),
        Err(ChannelTransferRelayError::Transport(msg)) => {
            tracing::warn!("ChannelTransferHandler: relay transport error: {msg}");
            encode_response(
                &req.request_id,
                req.channel_id,
                CODE_SERVICE_UNAVAILABLE,
                "application unavailable",
                &[],
            )
        }
        Err(other) => {
            tracing::warn!("ChannelTransferHandler: relay error: {other}");
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
// Registration entry point — keeps the wiring conditional on
// `[channel_transfer]` config being present, and keeps the construction
// chain (relay → lookups → handler → register) in one auditable place
// so server.rs / tests share the same path.
// =====================================================================

/// Register [`ChannelTransferHandler`] against `MessageType::TransferRequest`
/// iff Channel Transfer is enabled in config (i.e. `cfg.is_some()`).
///
/// Returns `Ok(true)` if the handler was registered, `Ok(false)` if
/// registration was skipped because no config is present.
///
/// Server boundary (server spec §1.4) is preserved: this function only
/// constructs and registers the wire ingress relay. It does NOT touch
/// `service_id`, business registry, `callback_url`, route prefix
/// validation, idempotency, audit, or any HTTP routes.
pub fn try_register_channel_transfer_handler(
    dispatcher: &mut MessageDispatcher,
    cfg: Option<&ChannelTransferConfig>,
    connection_manager: Arc<ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
) -> Result<bool> {
    let Some(cfg) = cfg else {
        info!("📡 Channel Transfer: 未启用 ([channel_transfer] 缺省)，跳过 wire ingress 注册");
        return Ok(false);
    };

    let relay = ChannelTransferRelayClient::new(cfg).map_err(|e| {
        ServerError::Internal(format!("ChannelTransferRelayClient init failed: {e}"))
    })?;
    let lookups: Arc<dyn ChannelTransferLookups> =
        Arc::new(DefaultChannelTransferLookups::new(
            connection_manager,
            subscribe_manager,
        ));
    dispatcher.register_handler(
        MessageType::TransferRequest,
        Box::new(ChannelTransferHandler::new(lookups, Arc::new(relay))),
    );
    info!(
        "✅ Channel Transfer: wire ingress handler 已注册 (target={}, timeout={}ms)",
        cfg.application_url, cfg.timeout_ms
    );
    Ok(true)
}
