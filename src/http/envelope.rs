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

//! HTTP API 统一响应信封
//!
//! 详细规范见 `docs/spec/02-server/SERVICE_RESPONSE_ENVELOPE_SPEC.md`。
//!
//! ## 形状
//!
//! 成功：
//! ```json
//! { "code": 0, "message": "OK", "data": { ... } }
//! ```
//!
//! 错误：
//! ```json
//! { "code": 20200, "message": "USER_NOT_FOUND: uid 999", "data": null }
//! ```
//!
//! ## 约束
//!
//! - `code` 必须是数字（JSON number），来源唯一是 [`privchat_protocol::ErrorCode`]
//! - `code = 0` 表示成功；`code != 0` 表示错误，此时 `data = null`
//! - HTTP status 仍保留 RESTful 语义（见 ENVELOPE_SPEC §3）
//! - 客户端**优先**根据 `body.code` 判断结果，HTTP status 是辅助
//!
//! ## 用法
//!
//! ```ignore
//! async fn create_user(...) -> ApiResult<serde_json::Value> {
//!     // ... 业务逻辑 ...
//!     Ok(ApiEnvelope::ok(serde_json::json!({
//!         "user_id": user.id,
//!         "created": true,
//!     })))
//! }
//! ```
//!
//! 错误路径直接返 `Err(ServerError::...)`；错误的 envelope 包装由
//! `impl IntoResponse for ServerError`（见 `crate::error`）自动完成。

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::error::Result;

/// HTTP API 统一响应信封
///
/// 所有 `/api/service/*` 与 `/api/admin/*` 的响应都使用本结构。
#[derive(Debug, Serialize)]
pub struct ApiEnvelope<T> {
    /// 错误码：0 表示成功，非 0 表示错误（来自 `protocol::ErrorCode`）
    pub code: u32,
    /// 简短错误描述；成功时为 "OK"
    pub message: String,
    /// 业务数据；错误时为 `null`
    pub data: Option<T>,
}

impl<T> ApiEnvelope<T> {
    /// 构造成功响应
    pub fn ok(data: T) -> Self {
        Self {
            code: 0,
            message: "OK".to_string(),
            data: Some(data),
        }
    }
}

impl ApiEnvelope<()> {
    /// 构造错误响应（不带 data；data 为 `None`）
    pub fn err(code: privchat_protocol::ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: protocol_code_value(code),
            message: message.into(),
            data: None,
        }
    }

    /// 通过 u32 直接构造错误响应（仅在已经从 `ServerError` 拿到映射后的 code 时用）
    pub fn err_raw(code: u32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl<T: Serialize> IntoResponse for ApiEnvelope<T> {
    fn into_response(self) -> Response {
        // 成功路径永远返 200；错误路径走 `IntoResponse for ServerError`
        // （由 axum 在 `Result<ApiEnvelope<T>, ServerError>` 自动 routing）。
        (StatusCode::OK, Json(self)).into_response()
    }
}

/// 取 `protocol::ErrorCode` 的数字值
///
/// 集中封装，避免 `as i32 as u32` 散落到 server 各处。
#[inline]
pub fn protocol_code_value(code: privchat_protocol::ErrorCode) -> u32 {
    code as u32
}

/// 类型别名：handler 返回类型统一用 `ApiResult<T>`。
///
/// `T` 是业务数据形状；错误一律走 `ServerError → ApiEnvelope<()>`。
pub type ApiResult<T> = Result<ApiEnvelope<T>>;
