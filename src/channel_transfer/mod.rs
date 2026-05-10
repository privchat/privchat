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

//! Channel Transfer — server-side relay utilities for the
//! `TransferRequest` / `TransferResponse` wire types
//! (spec `02-server/CHANNEL_TRANSFER_SPEC` v2.0).
//!
//! **Scope.** This module is *not* a business service. It hosts the
//! request/response extension to the existing channel pub/sub pipeline
//! (`SubscribeRequest` / `PublishRequest`). The wire ingress handler
//! (`handler::channel_transfer_handler`) and the `/api/service/transfer/send`
//! HTTP route both compose the helpers in here.
//!
//! Per spec §1.4, this module **MUST NOT** introduce:
//!
//!   - business `service_id` / service registry
//!   - `route` business dispatch
//!   - business idempotency or audit
//!
//! All of that lives in `neton-application-module-privchat`
//! (dispatch spec §1.4 Single Dispatch Authority).
//!
//! ## Layout
//!
//! - [`validation`]   — pure format validators (`request_id` / `route` / body size).
//! - [`types`]        — wire-adjacent JSON DTOs for the server↔application boundary.
//! - [`relay_client`] — outbound HTTP client for `POST /service/privchat/transfer/dispatch`.

pub mod relay_client;
pub mod types;
pub mod validation;

pub use relay_client::{ChannelTransferRelayClient, ChannelTransferRelayError};
pub use types::{ForwardTransferRequest, ForwardTransferResponse, SERVICE_KEY_HEADER};
pub use validation::{
    validate_transfer_body_size, validate_transfer_request_id, validate_transfer_route,
    ChannelTransferValidationError, MAX_TRANSFER_BODY_BYTES, MAX_TRANSFER_REQUEST_ID_LEN,
    MAX_TRANSFER_ROUTE_LEN,
};
