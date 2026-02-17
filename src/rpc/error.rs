use privchat_protocol::ErrorCode;
use std::fmt;

/// RPC 错误类型
///
/// 使用协议层的 ErrorCode 枚举，确保客户端和服务端使用统一的错误码
#[derive(Debug, Clone)]
pub struct RpcError {
    /// 协议层错误码
    pub code: ErrorCode,
    /// 错误消息
    pub message: String,
}

impl RpcError {
    /// 创建验证错误
    pub fn validation<S: Into<String>>(msg: S) -> Self {
        Self {
            code: ErrorCode::InvalidParams,
            message: msg.into(),
        }
    }

    /// 创建未授权错误
    pub fn unauthorized<S: Into<String>>(msg: S) -> Self {
        Self {
            code: ErrorCode::AuthRequired,
            message: msg.into(),
        }
    }

    /// 创建禁止访问错误
    pub fn forbidden<S: Into<String>>(msg: S) -> Self {
        Self {
            code: ErrorCode::PermissionDenied,
            message: msg.into(),
        }
    }

    /// 创建未找到错误
    pub fn not_found<S: Into<String>>(msg: S) -> Self {
        Self {
            code: ErrorCode::ResourceNotFound,
            message: msg.into(),
        }
    }

    /// 创建内部错误
    pub fn internal<S: Into<String>>(msg: S) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: msg.into(),
        }
    }

    /// 从协议层错误码创建
    pub fn from_code(code: ErrorCode, message: String) -> Self {
        Self { code, message }
    }

    /// 获取错误码数值
    pub fn code_value(&self) -> u32 {
        self.code.code()
    }

    /// 获取错误消息
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code.code(), self.message)
    }
}

impl std::error::Error for RpcError {}

/// RPC 结果类型
pub type RpcResult<T> = Result<T, RpcError>;

/// 从 ServerError 转换为 RpcError
impl From<crate::error::ServerError> for RpcError {
    fn from(err: crate::error::ServerError) -> Self {
        use privchat_protocol::ErrorCode;

        match err {
            crate::error::ServerError::Validation(msg) => RpcError::validation(msg),
            crate::error::ServerError::Authentication(msg) => RpcError::unauthorized(msg),
            crate::error::ServerError::Unauthorized(msg) => RpcError::unauthorized(msg),
            crate::error::ServerError::Authorization(msg) => RpcError::forbidden(msg),
            crate::error::ServerError::PermissionDenied(msg) => RpcError::forbidden(msg),
            crate::error::ServerError::UserNotFound(msg) => {
                RpcError::from_code(ErrorCode::UserNotFound, msg)
            }
            crate::error::ServerError::NotFound(msg) => RpcError::not_found(msg),
            crate::error::ServerError::MessageNotFound(msg) => {
                RpcError::from_code(ErrorCode::MessageNotFound, msg)
            }
            crate::error::ServerError::ChannelNotFound(msg) => {
                RpcError::from_code(ErrorCode::ChannelNotFound, msg)
            }
            _ => RpcError::internal(err.to_string()),
        }
    }
}
