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

//! Channel Transfer — server-side **wire-layer** utilities for the
//! `TransferRequest` / `TransferResponse` wire types
//! (spec `02-server/CHANNEL_TRANSFER_SPEC`).
//!
//! ## Scope after v1.0 refactor
//!
//! 本模块**只负责 wire 层**：
//!
//!   - decode wire `TransferRequest`
//!   - request_id / route / body size format validation
//!   - auth / channel access / room membership 校验
//!   - encode wire `TransferResponse`
//!
//! **server→application 出站不再使用专属 client**——v1.0 起 wire ingress handler
//! 把 `TransferRequest` 包装成 [`crate::server_event::ServerEvent`]
//!（`event_type = "transfer.requested"`）通过 [`crate::server_event::ServerEventClient`]
//! 投递；application 端的统一 `/service/privchat/server-event/dispatch` 入口分发。
//! 这条通道与所有其它 server emit 事件（`bot.followed` / `bot.unfollowed` / ...）
//! 共用同一 envelope（spec `SERVER_EVENT_DISPATCH_SPEC`）。
//!
//! Per spec §1.4，本模块**MUST NOT** 引入：
//!
//!   - business `service_id` / service registry
//!   - `route` 业务分发 / prefix↔service.name 校验
//!   - 业务幂等或审计
//!
//! 这些一律在 `neton-application-module-privchat` 端落地。
//!
//! ## Layout
//!
//! - [`validation`] — pure format validators (`request_id` / `route` / body size)。
//!
//! `relay_client.rs` 与 `types.rs` 在 v1.0 重构后已删除——server↔app 边界 DTO
//! 统一搬到 [`crate::server_event::types`]（`TransferRequestedPayload` /
//! `TransferResponsePayload`）。

pub mod validation;

pub use validation::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
    ChannelTransferValidationError, MAX_TRANSFER_BODY_BYTES, MAX_TRANSFER_REQUEST_ID_LEN,
    MAX_TRANSFER_ROUTE_LEN,
};
