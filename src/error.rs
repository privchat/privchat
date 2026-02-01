use std::fmt;
use std::error::Error as StdError;
use serde::{Serialize, Deserialize};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response, Json},
};

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
            ServerError::Unauthorized(msg) => write!(f, "Unauthorized: {}", msg),
            ServerError::Protocol(msg) => write!(f, "Protocol error: {}", msg),
            ServerError::BadRequest(msg) => write!(f, "Bad request: {}", msg),
            ServerError::NotFound(msg) => write!(f, "Not found: {}", msg),
            ServerError::Forbidden(msg) => write!(f, "Forbidden: {}", msg),
            ServerError::InvalidDeviceId => write!(f, "Invalid device ID"),
            ServerError::InvalidDeviceType => write!(f, "Invalid device type"),
            ServerError::DuplicateEntry(msg) => write!(f, "Duplicate entry: {}", msg),
        }
    }
}

impl StdError for ServerError {}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status_code = match &self {
            ServerError::Authentication(_) | ServerError::InvalidToken | ServerError::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            ServerError::Authorization(_) | ServerError::PermissionDenied(_) | ServerError::Forbidden(_) => StatusCode::FORBIDDEN,
            ServerError::Validation(_) | ServerError::InvalidRequest(_) | ServerError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ServerError::UserNotFound(_) | ServerError::ChannelNotFound(_) | ServerError::MessageNotFound(_) | ServerError::NotFound(_) => StatusCode::NOT_FOUND,
            ServerError::Duplicate(_) | ServerError::DuplicateEntry(_) => StatusCode::CONFLICT,
            ServerError::RateLimit(_) => StatusCode::TOO_MANY_REQUESTS,
            ServerError::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        
        let error_response = ErrorResponse::new(&self);
        (status_code, Json(error_response)).into_response()
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
            ServerError::Unauthorized(_) => ErrorCode::Authentication,
            ServerError::Protocol(_) => ErrorCode::Protocol,
            ServerError::BadRequest(_) => ErrorCode::BadRequest,
            ServerError::NotFound(_) => ErrorCode::NotFound,
            ServerError::Forbidden(_) => ErrorCode::Forbidden,
            ServerError::InvalidDeviceId => ErrorCode::InvalidDeviceId,
            ServerError::InvalidDeviceType => ErrorCode::InvalidDeviceType,
            ServerError::DuplicateEntry(_) => ErrorCode::DuplicateEntry,
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