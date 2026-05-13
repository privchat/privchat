// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! DTOs for `POST /service/privchat/service-account/event`
//! (spec `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §5).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Application-exposed sub-path that the server posts to.
/// The full URL is `<application_url><SERVICE_ACCOUNT_EVENT_PATH>`.
pub const SERVICE_ACCOUNT_EVENT_PATH: &str = "/service/privchat/service-account/event";

/// `event_type` enum — wire-encoded as snake_case string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAccountEventType {
    Followed,
    Unfollowed,
}

/// Server → application event payload (spec §5.2).
///
/// Bot user_type-only in v1.0. The endpoint path retains the umbrella
/// `service-account` name so future System / Official events can reuse it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountEvent {
    /// Unique transport id for server↔app dedupe.
    pub event_id: String,
    /// Cross-link trace id.
    pub trace_id: String,
    /// `followed` or `unfollowed`.
    pub event_type: ServiceAccountEventType,
    /// User who triggered the follow / unfollow.
    pub user_id: u64,
    /// Target Bot.
    pub bot_user_id: u64,
    /// Direct channel id between (user, bot).
    pub channel_id: u64,
    /// Reserved; v1.0 always `2` (Bot).
    pub account_user_type: i32,
    /// Only present on `followed` events:
    /// `true` = brand new or revived, `false` = idempotent reuse.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<bool>,
    /// Unix ms when the event happened on server.
    pub occurred_at: i64,
}

impl ServiceAccountEvent {
    pub fn followed(
        user_id: u64,
        bot_user_id: u64,
        channel_id: u64,
        account_user_type: i32,
        created: bool,
        occurred_at_ms: i64,
    ) -> Self {
        Self {
            event_id: format!("srv_evt_{}", Uuid::new_v4().simple()),
            trace_id: format!("trace_{}", Uuid::new_v4().simple()),
            event_type: ServiceAccountEventType::Followed,
            user_id,
            bot_user_id,
            channel_id,
            account_user_type,
            created: Some(created),
            occurred_at: occurred_at_ms,
        }
    }

    pub fn unfollowed(
        user_id: u64,
        bot_user_id: u64,
        channel_id: u64,
        account_user_type: i32,
        occurred_at_ms: i64,
    ) -> Self {
        Self {
            event_id: format!("srv_evt_{}", Uuid::new_v4().simple()),
            trace_id: format!("trace_{}", Uuid::new_v4().simple()),
            event_type: ServiceAccountEventType::Unfollowed,
            user_id,
            bot_user_id,
            channel_id,
            account_user_type,
            created: None,
            occurred_at: occurred_at_ms,
        }
    }
}

/// Application's flat-shaped ack (spec §5.3).
/// `accepted` is the dedupe-friendly truth value; `code` carries the
/// application's error code if the handler couldn't act on the event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceAccountEventAck {
    pub accepted: bool,
    pub code: i32,
    pub message: String,
}
