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

//! `POST /api/service/transfer/send` — application → server → user-sessions
//! channel transfer push (server spec §5.2 / §12.2).
//!
//! This route is **not** business dispatch — it is a packet-delivery extension
//! of the existing channel pub/sub pipeline (server spec §1.4). The
//! application has already done route prefix validation, idempotency, and
//! audit at `neton-application-module-privchat`; this handler:
//!
//!   1. authenticates the caller via the existing `verify_service_key`
//!      middleware (mounted under `/api/service/*`)
//!   2. validates the request shape (request_id / route / body)
//!   3. resolves channel → room
//!   4. checks subscription + room membership of `target_user_id`
//!   5. encodes a wire `TransferRequest` (biz_type 19) — `target_user_id`
//!      stays in the HTTP payload, **not** in the wire packet
//!   6. delivers to all of `target_user_id`'s authenticated sessions that
//!      are subscribed to `channel_id`
//!   7. returns an `ApiEnvelope<{accepted, delivered_sessions}>` per the
//!      response envelope spec
//!
//! Business outcomes are returned via [`TransferSendOutcome`]; the trait
//! [`TransferSendBackend`] abstracts the connection / subscription /
//! delivery surface so the pure pipeline ([`process_app_transfer_send`])
//! is unit-testable without standing up a real `ChatServer`.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::channel_transfer::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
};
use crate::http::envelope::ApiEnvelope;
use crate::http::AdminServerState;
use crate::infra::{ConnectionManager, SubscribeManager};

// =====================================================================
// Wire codes — spec §15
// =====================================================================

const CODE_INVALID_PARAMS: u32 = 10100;
const CODE_PERMISSION_DENIED: u32 = 10004;
const CODE_PAYLOAD_TOO_LARGE: u32 = 10106;
const CODE_INTERNAL_ERROR: u32 = 4;
const CODE_CHANNEL_NOT_FOUND: u32 = 20500;
const CODE_CHANNEL_NOT_SUBSCRIBED: u32 = 20900;

// =====================================================================
// Request / response DTOs (HTTP layer only — never reach the wire)
// =====================================================================

/// Inbound payload for `POST /api/service/transfer/send`.
///
/// `body` is base64-encoded UTF-8 text on the wire (server spec §5.2 +
/// envelope spec §2). `target_user_id` is the routing key for finding
/// recipient sessions; it is intentionally not propagated into the wire
/// `TransferRequest` packet emitted to those sessions.
#[derive(Debug, Clone, Deserialize)]
pub struct TransferSendRequest {
    pub request_id: String,
    pub channel_id: u64,
    pub target_user_id: u64,
    pub route: String,
    pub body: String,
}

/// `data` payload of the success envelope (spec §5.2):
///
/// ```json
/// { "code": 0, "message": "OK", "data": { "accepted": true, "delivered_sessions": N } }
/// ```
#[derive(Debug, Clone, Serialize)]
pub struct TransferSendOk {
    pub accepted: bool,
    pub delivered_sessions: usize,
}

/// Pipeline result for one HTTP call. Mapped to an `ApiEnvelope`-shaped
/// response by [`Self::into_response`].
#[derive(Debug)]
pub enum TransferSendOutcome {
    Ok(TransferSendOk),
    Err {
        code: u32,
        message: String,
        http_status: StatusCode,
    },
}

impl TransferSendOutcome {
    fn err(code: u32, message: impl Into<String>, http_status: StatusCode) -> Self {
        Self::Err {
            code,
            message: message.into(),
            http_status,
        }
    }
}

impl IntoResponse for TransferSendOutcome {
    fn into_response(self) -> Response {
        match self {
            TransferSendOutcome::Ok(data) => {
                (StatusCode::OK, Json(ApiEnvelope::ok(data))).into_response()
            }
            TransferSendOutcome::Err {
                code,
                message,
                http_status,
            } => (http_status, Json(ApiEnvelope::err_raw(code, message))).into_response(),
        }
    }
}

// =====================================================================
// Backend abstraction — keeps the pipeline testable without ChatServer
// =====================================================================

/// What the HTTP route needs from the rest of the server. Production wiring
/// (see [`DefaultTransferSendBackend`]) maps these onto `ConnectionManager`,
/// `SubscribeManager`, and the msgtrans `TransportServer`. Tests stub a
/// canned implementation.
#[async_trait]
pub trait TransferSendBackend: Send + Sync {
    /// Resolve `channel_id` → `room_id`. `None` ≡ channel does not exist
    /// (return `20500 ChannelNotFound`).
    async fn resolve_room(&self, channel_id: u64) -> Option<u64>;

    /// Whether `user_id` has *any* authenticated online session right now.
    /// Used to distinguish "offline → success(0)" from
    /// "online but unsubscribed → 20900".
    async fn is_user_online(&self, user_id: u64) -> bool;

    /// Whether `user_id` has at least one authenticated session subscribed
    /// to `channel_id`.
    async fn is_user_subscribed(&self, user_id: u64, channel_id: u64) -> bool;

    /// Whether `user_id` is a member of `room_id`. For Room channels in
    /// PrivChat's current model this is implied by subscription.
    async fn is_room_member(&self, user_id: u64, room_id: u64) -> bool;

    /// Deliver an already-encoded wire `TransferRequest` packet to all of
    /// `target_user_id`'s authenticated sessions that are subscribed to
    /// `channel_id`. Returns the number of sessions the packet was
    /// successfully sent to.
    ///
    /// `encoded_packet` is exactly what comes off
    /// `privchat_protocol::encode_message(&TransferRequest{...})` —
    /// implementations wrap it in a `msgtrans::packet::Packet` with
    /// `biz_type = MessageType::TransferRequest as u8` (= 19) and call the
    /// transport server.
    async fn deliver(
        &self,
        target_user_id: u64,
        channel_id: u64,
        encoded_packet: &[u8],
    ) -> usize;
}

// =====================================================================
// Production backend
// =====================================================================

pub struct DefaultTransferSendBackend {
    connection_manager: Arc<ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
}

impl DefaultTransferSendBackend {
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
impl TransferSendBackend for DefaultTransferSendBackend {
    async fn resolve_room(&self, channel_id: u64) -> Option<u64> {
        // Current PrivChat model: channel_id IS room_id. We do not query
        // ChannelService to verify existence here because the subscription
        // / online checks below are cheaper and equally protective: if the
        // channel doesn't exist nobody is subscribed to it.
        // TODO: when room and channel decouple, route through ChannelService.
        Some(channel_id)
    }

    async fn is_user_online(&self, user_id: u64) -> bool {
        !self
            .connection_manager
            .get_user_connections(user_id)
            .await
            .is_empty()
    }

    async fn is_user_subscribed(&self, user_id: u64, channel_id: u64) -> bool {
        let conns = self.connection_manager.get_user_connections(user_id).await;
        conns.iter().any(|c| {
            self.subscribe_manager
                .get_session_channels(&c.session_id)
                .contains(&channel_id)
        })
    }

    async fn is_room_member(&self, _user_id: u64, _room_id: u64) -> bool {
        // Phase 1: subscription presence is treated as proof of membership.
        // Subscribe-time gates already check Private/Group; Room has no
        // persistent membership in the current model.
        // TODO: defense-in-depth via ChannelService.get_channel_members for
        // the kicked-while-subscribed race.
        true
    }

    async fn deliver(
        &self,
        target_user_id: u64,
        channel_id: u64,
        encoded_packet: &[u8],
    ) -> usize {
        let conns = self
            .connection_manager
            .get_user_connections(target_user_id)
            .await;
        let target_sessions: Vec<msgtrans::SessionId> = conns
            .iter()
            .filter(|c| {
                self.subscribe_manager
                    .get_session_channels(&c.session_id)
                    .contains(&channel_id)
            })
            .map(|c| c.session_id)
            .collect();
        if target_sessions.is_empty() {
            return 0;
        }

        let transport = self.connection_manager.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            warn!(
                "TransferSend: TransportServer not initialized; dropping delivery to user {}",
                target_user_id
            );
            return 0;
        };

        let mut delivered = 0usize;
        for sid in target_sessions {
            let mut packet = msgtrans::packet::Packet::one_way(
                crate::infra::next_packet_id(),
                encoded_packet.to_vec(),
            );
            packet.set_biz_type(privchat_protocol::protocol::MessageType::TransferRequest as u8);
            match server.send_to_session(sid, packet).await {
                Ok(()) => delivered += 1,
                Err(e) => {
                    warn!(
                        "TransferSend: send_to_session failed user={} sid={} err={}",
                        target_user_id, sid, e
                    );
                }
            }
        }
        delivered
    }
}

// =====================================================================
// Pure pipeline (testable)
// =====================================================================

/// Run a parsed [`TransferSendRequest`] through the full validation +
/// delivery pipeline against the supplied [`TransferSendBackend`].
///
/// Does not check the X-Service-Key header — that runs ahead of this
/// function in the route handler.
pub async fn process_app_transfer_send<B: TransferSendBackend + ?Sized>(
    req: TransferSendRequest,
    backend: &B,
) -> TransferSendOutcome {
    // 1. Format gates (server spec §6 / §7 / §10).
    if let Err(e) = validate_transfer_request_id(&req.request_id) {
        return TransferSendOutcome::err(
            CODE_INVALID_PARAMS,
            e.to_string(),
            StatusCode::BAD_REQUEST,
        );
    }
    if let Err(e) = validate_transfer_route(&req.route) {
        return TransferSendOutcome::err(
            CODE_INVALID_PARAMS,
            e.to_string(),
            StatusCode::BAD_REQUEST,
        );
    }
    let body = match BASE64_STD.decode(req.body.as_bytes()) {
        Ok(b) => b,
        Err(e) => {
            return TransferSendOutcome::err(
                CODE_INVALID_PARAMS,
                format!("invalid base64 body: {e}"),
                StatusCode::BAD_REQUEST,
            );
        }
    };
    if let Err(e) = validate_transfer_body_size(&body) {
        return TransferSendOutcome::err(
            CODE_PAYLOAD_TOO_LARGE,
            e.to_string(),
            StatusCode::BAD_REQUEST,
        );
    }

    // 2. Resolve room.
    let Some(room_id) = backend.resolve_room(req.channel_id).await else {
        return TransferSendOutcome::err(
            CODE_CHANNEL_NOT_FOUND,
            "channel not found",
            StatusCode::NOT_FOUND,
        );
    };

    // 3. Subscription / online split.
    //    - subscribed       → proceed
    //    - online unsubscribed → 20900 (target online but not in this channel)
    //    - offline           → success with delivered_sessions = 0 (best-effort)
    let subscribed = backend
        .is_user_subscribed(req.target_user_id, req.channel_id)
        .await;
    if !subscribed {
        let online = backend.is_user_online(req.target_user_id).await;
        if online {
            return TransferSendOutcome::err(
                CODE_CHANNEL_NOT_SUBSCRIBED,
                "channel not subscribed",
                StatusCode::FORBIDDEN,
            );
        }
        // Offline → best-effort success with 0 delivered.
        return TransferSendOutcome::Ok(TransferSendOk {
            accepted: true,
            delivered_sessions: 0,
        });
    }

    // 4. Room membership.
    if !backend.is_room_member(req.target_user_id, room_id).await {
        return TransferSendOutcome::err(
            CODE_PERMISSION_DENIED,
            "room membership required",
            StatusCode::FORBIDDEN,
        );
    }

    // 5. Encode wire TransferRequest. target_user_id is intentionally NOT
    //    placed in the wire packet — receiving sessions know they are the
    //    target; the wire schema only carries request_id / channel_id /
    //    route / body (server spec §4.2).
    let wire_req = privchat_protocol::TransferRequest {
        request_id: req.request_id,
        channel_id: req.channel_id,
        route: req.route,
        body,
    };
    let encoded = match privchat_protocol::encode_message(&wire_req) {
        Ok(b) => b,
        Err(e) => {
            return TransferSendOutcome::err(
                CODE_INTERNAL_ERROR,
                format!("encode TransferRequest: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // 6. Deliver and report.
    let delivered = backend
        .deliver(req.target_user_id, req.channel_id, &encoded)
        .await;
    TransferSendOutcome::Ok(TransferSendOk {
        accepted: true,
        delivered_sessions: delivered,
    })
}

// =====================================================================
// Axum route
// =====================================================================

/// POST /send — actual handler. `verify_service_key` runs first; the rest
/// is a thin glue layer over [`process_app_transfer_send`].
async fn handle_transfer_send(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(req): Json<TransferSendRequest>,
) -> crate::error::Result<Response> {
    super::admin::verify_service_key(&headers, &state).await?;
    let backend = DefaultTransferSendBackend::new(
        state.connection_manager.clone(),
        state.subscribe_manager.clone(),
    );
    let outcome = process_app_transfer_send(req, &backend).await;
    Ok(outcome.into_response())
}

/// Mounted by `routes::create_admin_routes()` at `/api/service/transfer`.
pub fn create_route() -> Router<AdminServerState> {
    Router::new().route("/send", post(handle_transfer_send))
}
