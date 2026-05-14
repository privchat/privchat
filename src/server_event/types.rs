// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! DTOs for `POST /service/privchat/server-event/dispatch`
//! (spec `02-server/SERVER_EVENT_DISPATCH_SPEC` §4).
//!
//! Server Event 是 privchat-server → 下游的**统一 dispatch envelope**——
//! 所有 server 主动向 downstream emit 的请求（不论 ack-only 还是 request-response）
//! 都通过同一个 envelope 传输。envelope 顶层只携带传输元数据 + event_type +
//! base64 payload；payload 的 schema 由 event_type 决定。
//!
//! Event type 分两类：
//!
//! - **ack-only**（如 `bot.followed` / `bot.unfollowed`）：downstream 处理完返
//!   flat ack `{accepted, code, message}`，`response_payload` 为空。
//! - **request-response**（如 `transfer.requested`）：downstream 处理完返
//!   `{accepted, code, message, response_payload}`，`response_payload` 是 event-type
//!   特定的响应字节（base64）。
//!
//! 这条通道**与 channel / room 无关**。`channel_id` / `request_id` / `route` 等
//! 业务字段如果某种 event 需要，只能放在它自己的 payload 里（如
//! `TransferRequestedPayload.channel_id`），**不能**上升到 envelope 顶层。

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Application-exposed sub-path that the server posts to.
/// The full URL is `<application_url><SERVER_EVENT_DISPATCH_PATH>`.
pub const SERVER_EVENT_DISPATCH_PATH: &str = "/service/privchat/server-event/dispatch";

/// HTTP header carrying the master key on internal `/service/*` calls
/// (spec §13 / `SERVICE_API_SPEC` §1.4)。统一放这里，避免按 dispatch 入口
/// 各放一份导致命名漂移。
pub const SERVICE_KEY_HEADER: &str = "X-Service-Key";

// ───────────────────────── event_type constants ─────────────────────────
//
// 命名约定：`<domain>.<past_tense_action>`（spec §4.4）。
// 新增 event 在这里集中登记，便于下游订阅方一眼看全。

/// 用户 follow 一个 bot —— **ack-only**。Payload: [`BotFollowedPayload`]。
pub const EVENT_TYPE_BOT_FOLLOWED: &str = "bot.followed";

/// 用户 unfollow 一个 bot —— **ack-only**。Payload: [`BotUnfollowedPayload`]。
pub const EVENT_TYPE_BOT_UNFOLLOWED: &str = "bot.unfollowed";

/// Client wire `TransferRequest` 包装 —— **request-response**。
/// Payload: [`TransferRequestedPayload`]；ack `response_payload` 解码后为
/// [`TransferResponsePayload`]。详见 spec `CHANNEL_TRANSFER_SPEC` §5。
pub const EVENT_TYPE_TRANSFER_REQUESTED: &str = "transfer.requested";

/// 通用 server event envelope（spec §4.2）。
///
/// 顶层字段**只能**是传输元数据；业务字段全部在 [`payload`](Self::payload) 里。
/// 任何修改顶层字段集（加 `channel_id` / `route` / `request_id` 等）的 PR 都
/// 违背 spec §4 设计意图——拒绝。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEvent {
    /// server↔下游这一跳的 transport ID；与 `event_type` 组成幂等 PK。
    /// **MUST** server 端 UUID 生成，**不**复用 client 层任何 ID（如 transfer 的
    /// `request_id`）—— 两层幂等彼此独立。
    pub internal_event_id: String,
    /// 跨链路追踪 ID。
    pub trace_id: String,
    /// `domain.past_tense_action` 形态；下游按此字符串路由 handler。
    pub event_type: String,
    /// Base64 编码字节；schema 由 [`event_type`](Self::event_type) 决定。
    pub payload: String,
    /// Server 端事件发生时间 Unix ms。
    pub created_at: i64,
}

impl ServerEvent {
    /// Build an envelope from a serializable payload struct.
    pub fn new<P: Serialize>(
        event_type: &str,
        payload: &P,
        occurred_at_ms: i64,
    ) -> Result<Self, serde_json::Error> {
        let payload_bytes = serde_json::to_vec(payload)?;
        Ok(Self {
            internal_event_id: format!("srv_evt_{}", Uuid::new_v4().simple()),
            trace_id: format!("trace_{}", Uuid::new_v4().simple()),
            event_type: event_type.to_string(),
            payload: BASE64_STD.encode(&payload_bytes),
            created_at: occurred_at_ms,
        })
    }

    /// Convenience builder: `bot.followed`。
    pub fn bot_followed(
        user_id: u64,
        bot_user_id: u64,
        channel_id: u64,
        account_user_type: i32,
        created: bool,
        occurred_at_ms: i64,
    ) -> Result<Self, serde_json::Error> {
        let payload = BotFollowedPayload {
            user_id,
            bot_user_id,
            channel_id,
            account_user_type,
            created,
            occurred_at: occurred_at_ms,
        };
        Self::new(EVENT_TYPE_BOT_FOLLOWED, &payload, occurred_at_ms)
    }

    /// Convenience builder: `bot.unfollowed`。
    pub fn bot_unfollowed(
        user_id: u64,
        bot_user_id: u64,
        channel_id: u64,
        account_user_type: i32,
        occurred_at_ms: i64,
    ) -> Result<Self, serde_json::Error> {
        let payload = BotUnfollowedPayload {
            user_id,
            bot_user_id,
            channel_id,
            account_user_type,
            occurred_at: occurred_at_ms,
        };
        Self::new(EVENT_TYPE_BOT_UNFOLLOWED, &payload, occurred_at_ms)
    }

    /// Convenience builder: `transfer.requested`（request-response 类型）。
    ///
    /// `client_request_id` 来自 wire `TransferRequest.request_id`，进入 payload
    /// （downstream 用于 transfer 层 `(user_id, channel_id, client_request_id)`
    /// 幂等）。`internal_event_id` 仍由 [`Self::new`] 自动生成——两层幂等彼此独立。
    pub fn transfer_requested(
        client_request_id: String,
        user_id: u64,
        channel_id: u64,
        room_id: u64,
        route: String,
        body: Vec<u8>,
        occurred_at_ms: i64,
    ) -> Result<Self, serde_json::Error> {
        let payload = TransferRequestedPayload {
            request_id: client_request_id,
            user_id,
            channel_id,
            room_id,
            route,
            body: BASE64_STD.encode(&body),
        };
        Self::new(EVENT_TYPE_TRANSFER_REQUESTED, &payload, occurred_at_ms)
    }
}

/// Downstream 的 flat-shaped ack（spec §4.3）。
///
/// 顶层 `accepted` / `code` / `message` 表达 **server↔downstream 投递/调度结果**：
///
/// - `accepted=true, code=0` → 已被接收且 handler 已执行（业务结果在 `response_payload` 里，看 event_type）
/// - `accepted=true, code != 0` → application 当前不能处理（如 handler 未注册）
/// - `accepted=false, code != 0` → 系统级拒绝（鉴权 / payload 解码失败）
///
/// `response_payload`：仅 request-response 类型的 event 才返；ack-only 类型为
/// `None` / 空字符串。schema 由 `event_type` 决定（见各 event_type 的 payload 注释）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEventAck {
    pub accepted: bool,
    pub code: i32,
    pub message: String,
    /// Base64 编码字节；仅 request-response event 才返（如 `transfer.requested`
    /// 的 [`TransferResponsePayload`]）。`None` / `""` = 无 response payload。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_payload: Option<String>,
}

// ───────────────────────── payload schemas ─────────────────────────
//
// 每个 event_type 对应一个 payload struct。Downstream 按 event_type 决定怎么
// 反序列化 payload。所有字段都是 snake_case，与 envelope 风格一致。

/// Payload for `bot.followed`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotFollowedPayload {
    pub user_id: u64,
    pub bot_user_id: u64,
    /// Direct channel id between (user, bot)。便于 downstream 直接做 business_channel
    /// binding 与欢迎语；channel_id 出现在 payload 里**不**违反 spec §4.2——它是
    /// 这个 event 的业务字段，envelope 顶层依旧不知道 channel 概念。
    pub channel_id: u64,
    /// 当前 v1.0 固定 `2` (Bot)。保留以兼容未来扩展（System=1 / Official=3）。
    pub account_user_type: i32,
    /// `true` = 新建关系或从 unfollowed 复活；`false` = 已 followed 幂等复用。
    pub created: bool,
    pub occurred_at: i64,
}

/// Payload for `bot.unfollowed`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotUnfollowedPayload {
    pub user_id: u64,
    pub bot_user_id: u64,
    pub channel_id: u64,
    pub account_user_type: i32,
    pub occurred_at: i64,
}

/// Payload for `transfer.requested`（client wire `TransferRequest` 包装）。
///
/// 字段一一对应 client 看到的 wire 协议字段；下游 (application) 通过 `channel_id`
/// 解析 `privchat_business_channel`，再走 transfer 业务 dispatcher。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferRequestedPayload {
    /// Client 发起的请求 ID（wire `TransferRequest.request_id` 透传）。
    /// downstream 用于 transfer 层 `(user_id, channel_id, request_id)` 幂等。
    pub request_id: String,
    /// 发起方 user ID（server 从认证 session 注入）。
    pub user_id: u64,
    /// 目标 channel ID。
    pub channel_id: u64,
    /// channel 所在 room ID。当前 PrivChat 模型 `room_id == channel_id`，保留独立字段
    /// 供未来解耦。
    pub room_id: u64,
    /// Transfer route：`<service-name>/<module>/<action>`。
    pub route: String,
    /// Base64 编码业务 body 字节（raw ≤ 64 KB）。
    pub body: String,
}

/// Payload for `transfer.requested` 的 ack `response_payload`。
///
/// 字段对齐 wire `TransferResponse`：downstream（application）处理完后把业务
/// 结果填进来；server 端 wire ingress 拿到后直接编码成 wire `TransferResponse`
/// 回 client。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferResponsePayload {
    /// 与请求里的 `request_id` 一致（downstream 透传；空时 server 兜底用 client 原值）。
    pub request_id: String,
    /// 与请求里的 `channel_id` 一致。
    pub channel_id: u64,
    /// Transfer 业务结果 code（`0` = OK；其它见 `CHANNEL_TRANSFER_DISPATCH_SPEC` §12
    /// 的 20900-20909 段位）。**与 envelope 顶层的 `ServerEventAck.code` 不同语义**：
    /// 顶层 code 是 server↔app 投递/调度结果；这里是 transfer 业务结果。
    pub code: i32,
    pub message: String,
    /// Base64 编码业务响应字节。空字符串等价于"无 data"。
    #[serde(default)]
    pub data: String,
}
