// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Server Event — server-side outbound dispatcher for the **generic**
//! server→downstream event channel (spec `02-server/SERVER_EVENT_DISPATCH_SPEC`).
//!
//! 这是 privchat-server 定义的**通用 server→下游事件标准**，与 [`crate::channel_transfer`]
//! 是并行的两条 server↔下游 extension 通道：
//!
//! | | `channel_transfer` | `server_event` |
//! |---|---|---|
//! | 触发方 | client 主动发 `TransferRequest`（在某个 channel） | server 主动 emit 事件 |
//! | 绑 channel/room | **是** | **否** |
//! | 路由键 | `route` 第一段 = `service.name` | `event_type` 字符串 |
//! | 用途 | 频道内 client→app RPC | 用户行为 / bot 关系 / 消息持久化等 server 标准事件 |
//!
//! 任何 server 内部触发、需要通知下游的事件（bot.followed / bot.unfollowed /
//! channel.message_created / user.created / device.bound / ...）都从这条通道
//! 走——**不**为单个 event type 单开 webhook。
//!
//! ## Layout
//!
//! - [`types`]   — generic envelope + ack DTO + per-event payload structs；
//!                 `X-Service-Key` 复用 [`crate::channel_transfer::types::SERVICE_KEY_HEADER`]
//! - [`client`]  — outbound HTTP caller；warn-log on failure，no retry。
//!
//! 失败语义：best-effort per spec §6——warn-log，no retry，不回滚业务持久化。

pub mod client;
pub mod types;

pub use client::{ServerEventClient, ServerEventError};
pub use types::{
    BotFollowedPayload, BotUnfollowedPayload, ServerEvent, ServerEventAck,
    EVENT_TYPE_BOT_FOLLOWED, EVENT_TYPE_BOT_UNFOLLOWED, SERVER_EVENT_DISPATCH_PATH,
};
