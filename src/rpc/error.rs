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
            crate::error::ServerError::ChannelResyncRequired(msg) => {
                RpcError::from_code(ErrorCode::SyncChannelResyncRequired, msg)
            }
            crate::error::ServerError::EntityResyncRequired(msg) => {
                RpcError::from_code(ErrorCode::SyncEntityResyncRequired, msg)
            }
            crate::error::ServerError::FullRebuildRequired(msg) => {
                RpcError::from_code(ErrorCode::SyncFullRebuildRequired, msg)
            }
            // 🔴 「暂时不可用」必须原样传到客户端。压成 InternalError 会让两端 SDK
            // 把它当终局失败：不重试、直接回滚用户刚做的修改。
            // 割接停写窗口正是靠这个码告诉客户端「稍后再试」而不是「你改失败了」。
            crate::error::ServerError::ServiceUnavailable(msg) => {
                RpcError::from_code(ErrorCode::ServiceUnavailable, msg)
            }
            _ => RpcError::internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod service_error_mapping_tests {
    use super::*;
    use privchat_protocol::ErrorCode;

    /// 「暂时不可用」必须原样传到客户端，不能被压成 InternalError。
    ///
    /// 🔴 两端 SDK 的可重试白名单里有 `ServiceUnavailable(3)`，没有 `InternalError(4)`。
    /// 压成 4 之后，服务端说的「稍后再试」在客户端表现为「你这次修改失败了」——
    /// App 会直接回滚用户刚拨的开关，割接停写窗口期的所有修改就此丢掉。
    #[test]
    fn a_temporary_outage_stays_retryable_through_the_rpc_layer() {
        let mapped = RpcError::from(crate::error::ServerError::ServiceUnavailable(
            "隐私设置正在维护，请稍后重试".to_string(),
        ));
        assert_eq!(mapped.code, ErrorCode::ServiceUnavailable);
        assert_ne!(
            mapped.code,
            ErrorCode::InternalError,
            "压成 InternalError 会让客户端把可重试的故障当成终局拒绝",
        );
    }
}
