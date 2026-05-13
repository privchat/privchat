// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Service Account Event — server-side outbound webhook to the application
//! (spec `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §5).
//!
//! When a user follows / unfollows a Bot, the RPC handler (`account/bot/*`)
//! spawns a fire-and-forget task that posts a [`types::ServiceAccountEvent`]
//! to the application's `POST /service/privchat/service-account/event`
//! endpoint. The application then writes its own `privchat_business_channel`
//! binding and triggers any business `onFollow` / `onUnfollow` lifecycle
//! handler.
//!
//! ## Layout
//!
//! - [`types`]   — request / response DTOs + `X-Service-Key` header constant.
//! - [`client`]  — outbound HTTP caller; mirrors `channel_transfer::relay_client`.
//!
//! Failures are best-effort per spec §3.4: warn-log only, no retry, no
//! rollback of the follow relation.

pub mod client;
pub mod types;

pub use client::{ServiceAccountEventClient, ServiceAccountEventError};
pub use types::{ServiceAccountEvent, ServiceAccountEventAck, SERVICE_ACCOUNT_EVENT_PATH};
