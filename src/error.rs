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

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use std::error::Error as StdError;
use std::fmt;

/// 服务器错误类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServerError {
    /// 内部错误
    Internal(String),
    /// 认证错误
    Authentication(String),
    /// 授权错误
    Authorization(String),
    /// 验证错误
    Validation(String),
    /// 用户未找到
    UserNotFound(String),
    /// 会话未找到
    ChannelNotFound(String),
    /// 消息未找到
    MessageNotFound(String),
    /// 不在会话中
    NotInChannel(String),
    /// 数据库错误
    Database(String),
    /// 网络错误
    Network(String),
    /// 序列化错误
    Serialization(String),
    /// 配置错误
    Configuration(String),
    /// 缓存错误
    Cache(String),
    /// 限流错误
    RateLimit(String),
    /// 资源不足
    ResourceExhausted(String),
    /// 超时错误
    Timeout(String),
    /// 不支持的操作
    Unsupported(String),
    /// 无效的请求
    InvalidRequest(String),
    /// 重复的操作
    Duplicate(String),
    /// 权限不足
    PermissionDenied(String),
    /// 服务不可用
    ServiceUnavailable(String),
    /// 版本不匹配
    VersionMismatch(String),
    /// 未知错误
    Unknown(String),
    /// 无效令牌
    InvalidToken,
    /// 令牌已过期
    TokenExpired,
    /// 令牌已被撤销
    TokenRevoked,
    /// 未授权
    Unauthorized(String),
    /// 协议错误
    Protocol(String),
    /// 错误请求
    BadRequest(String),
    /// 资源未找到
    NotFound(String),
    /// 禁止访问
    Forbidden(String),
    /// 无效设备ID
    InvalidDeviceId,
    /// 无效设备类型
    InvalidDeviceType,
    /// 重复条目
    DuplicateEntry(String),
    /// 需要按 channel 做 scoped resync
    ChannelResyncRequired(String),
    /// 需要按 entity family 做 scoped resync
    EntityResyncRequired(String),
    /// 需要 full rebuild
    FullRebuildRequired(String),
}

impl fmt::Display for ServerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServerError::Internal(msg) => write!(f, "Internal error: {}", msg),
            ServerError::Authentication(msg) => write!(f, "Authentication error: {}", msg),
            ServerError::Authorization(msg) => write!(f, "Authorization error: {}", msg),
            ServerError::Validation(msg) => write!(f, "Validation error: {}", msg),
            ServerError::UserNotFound(id) => write!(f, "User not found: {}", id),
            ServerError::ChannelNotFound(id) => write!(f, "Channel not found: {}", id),
            ServerError::MessageNotFound(id) => write!(f, "Message not found: {}", id),
            ServerError::NotInChannel(id) => write!(f, "Not in channel: {}", id),
            ServerError::Database(msg) => write!(f, "Database error: {}", msg),
            ServerError::Network(msg) => write!(f, "Network error: {}", msg),
            ServerError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            ServerError::Configuration(msg) => write!(f, "Configuration error: {}", msg),
            ServerError::Cache(msg) => write!(f, "Cache error: {}", msg),
            ServerError::RateLimit(msg) => write!(f, "Rate limit error: {}", msg),
            ServerError::ResourceExhausted(msg) => write!(f, "Resource exhausted: {}", msg),
            ServerError::Timeout(msg) => write!(f, "Timeout error: {}", msg),
            ServerError::Unsupported(msg) => write!(f, "Unsupported operation: {}", msg),
            ServerError::InvalidRequest(msg) => write!(f, "Invalid request: {}", msg),
            ServerError::Duplicate(msg) => write!(f, "Duplicate operation: {}", msg),
            ServerError::PermissionDenied(msg) => write!(f, "Permission denied: {}", msg),
            ServerError::ServiceUnavailable(msg) => write!(f, "Service unavailable: {}", msg),
            ServerError::VersionMismatch(msg) => write!(f, "Version mismatch: {}", msg),
            ServerError::Unknown(msg) => write!(f, "Unknown error: {}", msg),
            ServerError::InvalidToken => write!(f, "Invalid token"),
            ServerError::TokenExpired => write!(f, "Token expired"),
            ServerError::TokenRevoked => write!(f, "Token revoked"),
            ServerError::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
            ServerError::Protocol(msg) => write!(f, "Protocol error: {}", msg),
            ServerError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            ServerError::NotFound(msg) => write!(f, "Not found: {}", msg),
            ServerError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            ServerError::InvalidDeviceId => write!(f, "Invalid device ID"),
            ServerError::InvalidDeviceType => write!(f, "Invalid device type"),
            ServerError::DuplicateEntry(msg) => write!(f, "Duplicate entry: {}", msg),
            ServerError::ChannelResyncRequired(msg) => {
                write!(f, "Channel resync required: {}", msg)
            }
            ServerError::EntityResyncRequired(msg) => {
                write!(f, "Entity resync required: {}", msg)
            }
            ServerError::FullRebuildRequired(msg) => write!(f, "Full rebuild required: {}", msg),
        }
    }
}

impl StdError for ServerError {}

impl ServerError {
    /// 映射到 `protocol::ErrorCode`（数字码，跨 HTTP/RPC/TCP 唯一来源）。
    ///
    /// 完整映射表见 `docs/spec/02-server/SERVICE_RESPONSE_ENVELOPE_SPEC.md` §4.1。
    /// `Validation` / `DuplicateEntry` 会根据 message 前缀进一步分流（见 §4.2）。
    pub fn protocol_code(&self) -> privchat_protocol::ErrorCode {
        use privchat_protocol::ErrorCode as P;
        match self {
            ServerError::Internal(_) => P::InternalError,
            ServerError::Authentication(_) => P::AuthRequired,
            ServerError::Authorization(_) => P::PermissionDenied,
            ServerError::Validation(msg) => {
                subcode_from_message(msg).unwrap_or(P::InvalidParams)
            }
            ServerError::UserNotFound(_) => P::UserNotFound,
            ServerError::ChannelNotFound(_) => P::ChannelNotFound,
            ServerError::MessageNotFound(_) => P::MessageNotFound,
            ServerError::NotInChannel(_) => P::OperationNotAllowed,
            ServerError::Database(_) => P::DatabaseError,
            ServerError::Network(_) => P::NetworkError,
            ServerError::Serialization(_) => P::EncodingError,
            ServerError::Configuration(_) => P::InternalError,
            ServerError::Cache(_) => P::CacheError,
            ServerError::RateLimit(_) => P::RateLimitExceeded,
            ServerError::ResourceExhausted(_) => P::ConcurrentLimitExceeded,
            ServerError::Timeout(_) => P::Timeout,
            ServerError::Unsupported(_) => P::OperationNotAllowed,
            ServerError::InvalidRequest(_) => P::InvalidParams,
            ServerError::Duplicate(_) => P::OperationConflict,
            ServerError::PermissionDenied(_) => P::PermissionDenied,
            ServerError::ServiceUnavailable(_) => P::ServiceUnavailable,
            ServerError::VersionMismatch(_) => P::VersionError,
            ServerError::Unknown(_) => P::SystemError,
            ServerError::InvalidToken => P::InvalidToken,
            ServerError::TokenExpired => P::TokenExpired,
            ServerError::TokenRevoked => P::TokenRevoked,
            ServerError::Unauthorized(_) => P::AuthRequired,
            ServerError::Protocol(_) => P::ProtocolError,
            ServerError::BadRequest(_) => P::InvalidParams,
            ServerError::NotFound(msg) => {
                subcode_from_message(msg).unwrap_or(P::ResourceNotFound)
            }
            ServerError::Forbidden(_) => P::PermissionDenied,
            ServerError::InvalidDeviceId => P::InvalidParams,
            ServerError::InvalidDeviceType => P::InvalidParams,
            ServerError::DuplicateEntry(msg) => {
                subcode_from_message(msg).unwrap_or(P::OperationConflict)
            }
            ServerError::ChannelResyncRequired(_) => P::SyncChannelResyncRequired,
            ServerError::EntityResyncRequired(_) => P::SyncEntityResyncRequired,
            ServerError::FullRebuildRequired(_) => P::SyncFullRebuildRequired,
        }
    }

    /// 选择合适的 HTTP status code（见 ENVELOPE_SPEC §3.1 / §4.1）。
    pub fn http_status(&self) -> StatusCode {
        match self {
            ServerError::Authentication(_)
            | ServerError::InvalidToken
            | ServerError::TokenExpired
            | ServerError::TokenRevoked
            | ServerError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ServerError::Authorization(_)
            | ServerError::PermissionDenied(_)
            | ServerError::Forbidden(_)
            | ServerError::NotInChannel(_)
            | ServerError::Unsupported(_) => StatusCode::FORBIDDEN,
            ServerError::Validation(_)
            | ServerError::InvalidRequest(_)
            | ServerError::BadRequest(_)
            | ServerError::InvalidDeviceId
            | ServerError::InvalidDeviceType
            | ServerError::VersionMismatch(_) => StatusCode::BAD_REQUEST,
            ServerError::UserNotFound(_)
            | ServerError::ChannelNotFound(_)
            | ServerError::MessageNotFound(_)
            | ServerError::NotFound(_) => StatusCode::NOT_FOUND,
            ServerError::Duplicate(_)
            | ServerError::DuplicateEntry(_)
            | ServerError::ChannelResyncRequired(_)
            | ServerError::EntityResyncRequired(_)
            | ServerError::FullRebuildRequired(_) => StatusCode::CONFLICT,
            ServerError::RateLimit(_) | ServerError::ResourceExhausted(_) => {
                StatusCode::TOO_MANY_REQUESTS
            }
            ServerError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ServerError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// 从错误 message 前缀分流到具体 `protocol::ErrorCode`（见 ENVELOPE_SPEC §4.2）。
///
/// USER_API §3 / TOKEN_API 等约定调用方写 `<TAG>: <detail>` 形式的 message。
/// 本函数识别这些 tag 并返对应精确码；不识别返 `None`，由调用 variant 走默认映射。
fn subcode_from_message(msg: &str) -> Option<privchat_protocol::ErrorCode> {
    use privchat_protocol::ErrorCode as P;
    // 兼容 Display 加了 "<Variant Display>: " 前缀的情形
    let trimmed = msg
        .strip_prefix("Validation error: ")
        .or_else(|| msg.strip_prefix("Not found: "))
        .or_else(|| msg.strip_prefix("Duplicate entry: "))
        .unwrap_or(msg);
    if trimmed.starts_with("INVALID_USER_IDENTITY") {
        Some(P::MissingRequiredParam)
    } else if trimmed.starts_with("INVALID_PHONE_FORMAT") {
        Some(P::InvalidFormat)
    } else if trimmed.starts_with("USER_NOT_FOUND") {
        Some(P::UserNotFound)
    } else if trimmed.starts_with("IDENTITY_CONFLICT") {
        Some(P::OperationConflict)
    } else {
        None
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status_code = self.http_status();
        let code = self.protocol_code() as u32;
        let envelope = serde_json::json!({
            "code": code,
            "message": self.to_string(),
            "data": null,
        });
        (status_code, Json(envelope)).into_response()
    }
}

impl From<std::io::Error> for ServerError {
    fn from(err: std::io::Error) -> Self {
        ServerError::Internal(err.to_string())
    }
}

impl From<serde_json::Error> for ServerError {
    fn from(err: serde_json::Error) -> Self {
        ServerError::Serialization(err.to_string())
    }
}

impl From<tokio::time::error::Elapsed> for ServerError {
    fn from(err: tokio::time::error::Elapsed) -> Self {
        ServerError::Timeout(err.to_string())
    }
}

/// 结果类型别名
pub type Result<T> = std::result::Result<T, ServerError>;

/// 数据库错误类型别名
pub type DatabaseError = ServerError;

/// 应用错误类型别名
pub type AppError = ServerError;

/// 错误代码
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// 成功
    Success = 0,
    /// 内部错误
    Internal = 1000,
    /// 认证错误
    Authentication = 1001,
    /// 授权错误
    Authorization = 1002,
    /// 验证错误
    Validation = 1003,
    /// 用户未找到
    UserNotFound = 1004,
    /// 会话未找到
    ChannelNotFound = 1005,
    /// 消息未找到
    MessageNotFound = 1006,
    /// 不在会话中
    NotInChannel = 1007,
    /// 数据库错误
    Database = 2000,
    /// 网络错误
    Network = 2001,
    /// 序列化错误
    Serialization = 2002,
    /// 配置错误
    Configuration = 2003,
    /// 缓存错误
    Cache = 2004,
    /// 限流错误
    RateLimit = 3000,
    /// 资源不足
    ResourceExhausted = 3001,
    /// 超时错误
    Timeout = 3002,
    /// 不支持的操作
    Unsupported = 4000,
    /// 无效的请求
    InvalidRequest = 4001,
    /// 重复的操作
    Duplicate = 4002,
    /// 权限不足
    PermissionDenied = 4003,
    /// 服务不可用
    ServiceUnavailable = 5000,
    /// 版本不匹配
    VersionMismatch = 5001,
    /// 无效令牌
    InvalidToken = 5002,
    /// 令牌已过期
    TokenExpired = 5010,
    /// 令牌已被撤销
    TokenRevoked = 5011,
    /// 协议错误
    Protocol = 5003,
    /// 错误请求
    BadRequest = 5004,
    /// 资源未找到
    NotFound = 5005,
    /// 禁止访问
    Forbidden = 5006,
    /// 无效设备ID
    InvalidDeviceId = 5007,
    /// 无效设备类型
    InvalidDeviceType = 5008,
    /// 重复条目
    DuplicateEntry = 5009,
    /// 未知错误
    Unknown = 9999,
}

impl From<&ServerError> for ErrorCode {
    fn from(error: &ServerError) -> Self {
        match error {
            ServerError::Internal(_) => ErrorCode::Internal,
            ServerError::Authentication(_) => ErrorCode::Authentication,
            ServerError::Authorization(_) => ErrorCode::Authorization,
            ServerError::Validation(_) => ErrorCode::Validation,
            ServerError::UserNotFound(_) => ErrorCode::UserNotFound,
            ServerError::ChannelNotFound(_) => ErrorCode::ChannelNotFound,
            ServerError::MessageNotFound(_) => ErrorCode::MessageNotFound,
            ServerError::NotInChannel(_) => ErrorCode::NotInChannel,
            ServerError::Database(_) => ErrorCode::Database,
            ServerError::Network(_) => ErrorCode::Network,
            ServerError::Serialization(_) => ErrorCode::Serialization,
            ServerError::Configuration(_) => ErrorCode::Configuration,
            ServerError::Cache(_) => ErrorCode::Cache,
            ServerError::RateLimit(_) => ErrorCode::RateLimit,
            ServerError::ResourceExhausted(_) => ErrorCode::ResourceExhausted,
            ServerError::Timeout(_) => ErrorCode::Timeout,
            ServerError::Unsupported(_) => ErrorCode::Unsupported,
            ServerError::InvalidRequest(_) => ErrorCode::InvalidRequest,
            ServerError::Duplicate(_) => ErrorCode::Duplicate,
            ServerError::PermissionDenied(_) => ErrorCode::PermissionDenied,
            ServerError::ServiceUnavailable(_) => ErrorCode::ServiceUnavailable,
            ServerError::VersionMismatch(_) => ErrorCode::VersionMismatch,
            ServerError::Unknown(_) => ErrorCode::Unknown,
            ServerError::InvalidToken => ErrorCode::InvalidToken,
            ServerError::TokenExpired => ErrorCode::TokenExpired,
            ServerError::TokenRevoked => ErrorCode::TokenRevoked,
            ServerError::Unauthorized(_) => ErrorCode::Authentication,
            ServerError::Protocol(_) => ErrorCode::Protocol,
            ServerError::BadRequest(_) => ErrorCode::BadRequest,
            ServerError::NotFound(_) => ErrorCode::NotFound,
            ServerError::Forbidden(_) => ErrorCode::Forbidden,
            ServerError::InvalidDeviceId => ErrorCode::InvalidDeviceId,
            ServerError::InvalidDeviceType => ErrorCode::InvalidDeviceType,
            ServerError::DuplicateEntry(_) => ErrorCode::DuplicateEntry,
            ServerError::ChannelResyncRequired(_) => ErrorCode::Duplicate,
            ServerError::EntityResyncRequired(_) => ErrorCode::Duplicate,
            ServerError::FullRebuildRequired(_) => ErrorCode::Duplicate,
        }
    }
}

/// 错误响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// 错误代码
    pub code: ErrorCode,
    /// 错误消息
    pub message: String,
    /// 详细信息
    pub details: Option<String>,
    /// 时间戳
    pub timestamp: u64,
}

impl ErrorResponse {
    /// 创建错误响应
    pub fn new(error: &ServerError) -> Self {
        Self {
            code: ErrorCode::from(error),
            message: error.to_string(),
            details: None,
            timestamp: chrono::Utc::now().timestamp() as u64,
        }
    }

    /// 创建带详细信息的错误响应
    pub fn with_details(error: &ServerError, details: String) -> Self {
        Self {
            code: ErrorCode::from(error),
            message: error.to_string(),
            details: Some(details),
            timestamp: chrono::Utc::now().timestamp() as u64,
        }
    }
}
