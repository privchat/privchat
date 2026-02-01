use serde::{Deserialize, Serialize};
use serde_json::Value;

/// RPC 请求消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RPCMessageRequest {
    /// 路由路径，格式：system/module/action
    pub route: String,
    /// 请求参数 JSON
    pub body: Value,
}

/// RPC 响应消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RPCMessageResponse {
    /// 状态码
    pub code: i32,
    /// 响应消息
    pub message: String,
    /// 响应数据
    pub data: Option<Value>,
}

impl RPCMessageResponse {
    /// 创建成功响应
    /// 
    /// RPC 层不是 HTTP，code == 0 永远表示 success
    pub fn success(data: Value) -> Self {
        Self {
            code: 0,
            message: "OK".to_string(),
            data: Some(data),
        }
    }

    /// 创建成功响应（无数据）
    /// 
    /// RPC 层不是 HTTP，code == 0 永远表示 success
    pub fn success_empty() -> Self {
        Self {
            code: 0,
            message: "OK".to_string(),
            data: None,
        }
    }

    /// 创建错误响应
    /// 
    /// code != 0 表示错误，常见的错误码：
    /// - 400: ValidationError
    /// - 401: Unauthorized
    /// - 403: Forbidden
    /// - 404: NotFound
    /// - 500: InternalError
    pub fn error(code: i32, message: String) -> Self {
        Self {
            code,
            message,
            data: None,
        }
    }

    /// 检查响应是否成功
    /// 
    /// # 返回
    /// - `true`: code == 0，表示成功
    /// - `false`: code != 0，表示错误
    #[inline]
    pub fn is_ok(&self) -> bool {
        self.code == 0
    }

    /// 检查响应是否失败
    /// 
    /// # 返回
    /// - `true`: code != 0，表示错误
    /// - `false`: code == 0，表示成功
    #[inline]
    pub fn is_err(&self) -> bool {
        self.code != 0
    }
} 