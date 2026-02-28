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

//! 认证中间件

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use std::sync::Arc;
use tracing::debug;

use crate::service::FileService;

/// 认证中间件（暂时简单实现，未来可以验证 JWT token）
pub async fn auth_middleware(
    State(_file_service): State<Arc<FileService>>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // TODO: 实现真正的认证逻辑
    // 1. 从 Authorization header 提取 token
    // 2. 验证 token 有效性
    // 3. 提取 user_id 并添加到 request extensions

    debug!("🔐 认证中间件（暂时跳过验证）");

    // 暂时跳过认证，直接继续
    Ok(next.run(request).await)
}
