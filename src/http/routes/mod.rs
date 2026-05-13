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

//! HTTP 路由模块
//!
//! 路由结构：
//! - 文件服务（端口 9083，对外）：
//!   - `/api/app/files/upload` - 文件上传
//!   - `/metrics` - Prometheus 指标
//! - 管理 API（端口 9090，仅内网）：
//!   - `/api/admin/*` - 管理接口 legacy 前缀（向后兼容）
//!   - `/api/service/*` - 管理接口 v1.2 前缀（与 legacy 完全等价）

pub mod admin;
pub mod auth;
pub mod auth_jwks;
pub mod metrics;
pub mod room_tickets;
pub mod transfer;
pub mod upload;

use crate::http::{AdminServerState, FileServerState};
use axum::{routing::get, Router};

/// 创建文件服务路由
pub fn create_file_routes() -> Router<FileServerState> {
    Router::new()
        .route("/metrics", get(metrics::metrics_handler))
        .merge(upload::create_route())
}

/// 创建管理 API 路由
///
/// 同 handler 同时挂载到 `/api/admin` 和 `/api/service` 两个前缀下，行为完全一致。
/// `/api/admin` 是 v1.0/v1.1 兼容前缀；`/api/service` 是 v1.2 推荐前缀。
///
/// `/api/service/auth/*` 是 spec TOKEN_UNIFICATION_SPEC v1.3 §6.1 unified token API：
/// - `/auth/issue|refresh|introspect|revoke` 走 service-key（在 handler 里 `verify_service_key`）
/// - `/auth/jwks` **不**走 service master key（公钥发布，安全边界在私钥）
pub fn create_admin_routes() -> Router<AdminServerState> {
    Router::new()
        .nest("/api/admin", admin::create_route())
        .nest("/api/service", admin::create_route())
        .nest("/api/service/auth", auth::create_route())
        .route("/api/service/auth/jwks", get(auth_jwks::handler))
        // Channel Transfer push endpoint (server spec §5.2). Inherits the
        // existing X-Service-Key check from `/api/service/*` via the route
        // handler's explicit `verify_service_key` call. NOT a business
        // dispatch — pure packet-delivery extension of channel pub/sub
        // (server spec §1.4).
        .nest("/api/service/transfer", transfer::create_route())
        // Room subscribe ticket issuer (spec ROOM_CHANNEL_SPEC §4.5). Same
        // X-Service-Key gate; business APIs call this after deciding the
        // requesting user should be admitted to a Room channel.
        .nest("/api/service/room-tickets", room_tickets::create_route())
}
