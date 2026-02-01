use std::collections::HashMap;
use std::net::SocketAddr;
use msgtrans::SessionId;

/// 请求上下文
#[derive(Debug, Clone)]
pub struct RequestContext {
    /// 会话ID
    pub session_id: SessionId,
    /// 消息数据
    pub data: Vec<u8>,
    /// 客户端地址
    pub remote_addr: SocketAddr,
    /// 用户ID (可选)
    pub user_id: Option<String>,
    /// 设备ID (可选)
    pub device_id: Option<String>,
    /// 请求时间戳
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// 扩展属性
    pub attributes: HashMap<String, String>,
}

impl RequestContext {
    /// 创建新的请求上下文
    pub fn new(session_id: SessionId, data: Vec<u8>, remote_addr: SocketAddr) -> Self {
        Self {
            session_id,
            data,
            remote_addr,
            user_id: None,
            device_id: None,
            timestamp: chrono::Utc::now(),
            attributes: HashMap::new(),
        }
    }
    
    /// 设置用户ID
    pub fn with_user_id(mut self, user_id: String) -> Self {
        self.user_id = Some(user_id);
        self
    }
    
    /// 设置设备ID
    pub fn with_device_id(mut self, device_id: String) -> Self {
        self.device_id = Some(device_id);
        self
    }
    
    /// 添加属性
    pub fn with_attribute(mut self, key: String, value: String) -> Self {
        self.attributes.insert(key, value);
        self
    }
    
    /// 获取属性
    pub fn get_attribute(&self, key: &str) -> Option<&String> {
        self.attributes.get(key)
    }
    
    /// 是否已认证
    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
    
    /// 获取数据的 JSON 表示
    pub fn data_as_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        let json_str = String::from_utf8_lossy(&self.data);
        serde_json::from_str(&json_str)
    }
    
    /// 获取数据为字符串
    pub fn data_as_string(&self) -> String {
        String::from_utf8_lossy(&self.data).to_string()
    }
}

/// 响应构建器
#[derive(Debug, Clone)]
pub struct ResponseBuilder {
    data: Vec<u8>,
    session_id: SessionId,
}

impl ResponseBuilder {
    /// 创建新的响应构建器
    pub fn new(session_id: SessionId) -> Self {
        Self {
            data: Vec::new(),
            session_id,
        }
    }
    
    /// 从 JSON 构建响应
    pub fn from_json(session_id: SessionId, json: serde_json::Value) -> Result<Self, serde_json::Error> {
        let data = serde_json::to_vec(&json)?;
        Ok(Self { data, session_id })
    }
    
    /// 从字符串构建响应
    pub fn from_string(session_id: SessionId, s: String) -> Self {
        Self {
            data: s.into_bytes(),
            session_id,
        }
    }
    
    /// 构建响应数据
    pub fn build(self) -> Vec<u8> {
        self.data
    }
}

/// 错误响应构建器
pub struct ErrorResponseBuilder;

impl ErrorResponseBuilder {
    /// 创建错误响应
    pub fn build(session_id: SessionId, error_code: &str, error_message: &str) -> Vec<u8> {
        let response = serde_json::json!({
            "type": "error",
            "session_id": session_id.to_string(),
            "error": {
                "code": error_code,
                "message": error_message
            },
            "timestamp": chrono::Utc::now().to_rfc3339()
        });
        
        serde_json::to_vec(&response).unwrap_or_default()
    }
    
    /// 创建协议错误响应
    pub fn protocol_error(session_id: SessionId, message: &str) -> Vec<u8> {
        Self::build(session_id, "PROTOCOL_ERROR", message)
    }
    
    /// 创建认证错误响应
    pub fn auth_error(session_id: SessionId, message: &str) -> Vec<u8> {
        Self::build(session_id, "AUTH_ERROR", message)
    }
    
    /// 创建服务器错误响应
    pub fn server_error(session_id: SessionId, message: &str) -> Vec<u8> {
        Self::build(session_id, "SERVER_ERROR", message)
    }
} 