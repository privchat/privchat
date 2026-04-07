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
//!   - `/api/admin/*` - 管理接口（业务系统调用，使用 X-Service-Key 认证）

pub mod admin;
pub mod metrics;
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
pub fn create_admin_routes() -> Router<AdminServerState> {
    admin::create_route()
}
