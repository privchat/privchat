/// 服务间通信协议定义
/// 
/// 定义了服务间通信的标准消息格式，包括：
/// - 请求/响应消息结构
/// - 错误处理
/// - 序列化/反序列化
/// - 消息类型管理

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use uuid::Uuid;
use chrono::{DateTime, Utc};

/// 服务间通信的标准请求格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceRequest {
    /// 请求ID（用于追踪和匹配响应）
    pub request_id: String,
    /// 请求的服务名称
    pub service_name: String,
    /// 请求的方法/操作
    pub method: String,
    /// 请求版本
    pub version: String,
    /// 请求时间戳
    pub timestamp: DateTime<Utc>,
    /// 请求来源服务
    pub source_service: String,
    /// 请求数据
    pub data: serde_json::Value,
    /// 请求头部信息
    pub headers: HashMap<String, String>,
    /// 超时时间（秒）
    pub timeout: Option<u64>,
    /// 重试次数
    pub retry_count: Option<u32>,
}

impl ServiceRequest {
    /// 创建新的服务请求
    pub fn new(service_name: String, method: String, source_service: String) -> Self {
        Self {
            request_id: Uuid::new_v4().to_string(),
            service_name,
            method,
            version: "1.0.0".to_string(),
            timestamp: Utc::now(),
            source_service,
            data: serde_json::Value::Null,
            headers: HashMap::new(),
            timeout: Some(30),
            retry_count: Some(3),
        }
    }
    
    /// 设置请求数据
    pub fn with_data<T: Serialize>(mut self, data: T) -> Result<Self, serde_json::Error> {
        self.data = serde_json::to_value(data)?;
        Ok(self)
    }
    
    /// 设置请求头
    pub fn with_header(mut self, key: String, value: String) -> Self {
        self.headers.insert(key, value);
        self
    }
    
    /// 设置超时时间
    pub fn with_timeout(mut self, timeout_seconds: u64) -> Self {
        self.timeout = Some(timeout_seconds);
        self
    }
    
    /// 设置重试次数
    pub fn with_retry(mut self, retry_count: u32) -> Self {
        self.retry_count = Some(retry_count);
        self
    }
    
    /// 获取请求数据
    pub fn get_data<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }
    
    /// 序列化为字节数组
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
    
    /// 从字节数组反序列化
    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

/// 服务间通信的标准响应格式
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceResponse {
    /// 对应的请求ID
    pub request_id: String,
    /// 响应状态码
    pub status_code: u16,
    /// 响应状态描述
    pub status_message: String,
    /// 响应时间戳
    pub timestamp: DateTime<Utc>,
    /// 响应服务名称
    pub service_name: String,
    /// 响应数据
    pub data: serde_json::Value,
    /// 响应头部信息
    pub headers: HashMap<String, String>,
    /// 错误信息（如果有）
    pub error: Option<ServiceError>,
    /// 处理耗时（毫秒）
    pub processing_time: Option<u64>,
}

impl ServiceResponse {
    /// 创建成功响应
    pub fn success(request_id: String, service_name: String) -> Self {
        Self {
            request_id,
            status_code: 200,
            status_message: "OK".to_string(),
            timestamp: Utc::now(),
            service_name,
            data: serde_json::Value::Null,
            headers: HashMap::new(),
            error: None,
            processing_time: None,
        }
    }
    
    /// 创建错误响应
    pub fn error(request_id: String, service_name: String, error: ServiceError) -> Self {
        Self {
            request_id,
            status_code: error.code,
            status_message: error.message.clone(),
            timestamp: Utc::now(),
            service_name,
            data: serde_json::Value::Null,
            headers: HashMap::new(),
            error: Some(error),
            processing_time: None,
        }
    }
    
    /// 设置响应数据
    pub fn with_data<T: Serialize>(mut self, data: T) -> Result<Self, serde_json::Error> {
        self.data = serde_json::to_value(data)?;
        Ok(self)
    }
    
    /// 设置响应头
    pub fn with_header(mut self, key: String, value: String) -> Self {
        self.headers.insert(key, value);
        self
    }
    
    /// 设置处理时间
    pub fn with_processing_time(mut self, processing_time: u64) -> Self {
        self.processing_time = Some(processing_time);
        self
    }
    
    /// 获取响应数据
    pub fn get_data<T: for<'de> Deserialize<'de>>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.data.clone())
    }
    
    /// 检查是否成功
    pub fn is_success(&self) -> bool {
        self.status_code >= 200 && self.status_code < 300
    }
    
    /// 序列化为字节数组
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
    
    /// 从字节数组反序列化
    pub fn from_bytes(data: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(data)
    }
}

/// 服务错误信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceError {
    /// 错误代码
    pub code: u16,
    /// 错误消息
    pub message: String,
    /// 错误类型
    pub error_type: String,
    /// 错误详情
    pub details: HashMap<String, serde_json::Value>,
    /// 堆栈跟踪（开发环境）
    pub stack_trace: Option<String>,
}

impl ServiceError {
    /// 创建新的服务错误
    pub fn new(code: u16, message: String, error_type: String) -> Self {
        Self {
            code,
            message,
            error_type,
            details: HashMap::new(),
            stack_trace: None,
        }
    }
    
    /// 创建客户端错误（400-499）
    pub fn client_error(message: String) -> Self {
        Self::new(400, message, "CLIENT_ERROR".to_string())
    }
    
    /// 创建服务器错误（500-599）
    pub fn server_error(message: String) -> Self {
        Self::new(500, message, "SERVER_ERROR".to_string())
    }
    
    /// 创建权限错误
    pub fn permission_denied(message: String) -> Self {
        Self::new(403, message, "PERMISSION_DENIED".to_string())
    }
    
    /// 创建未找到错误
    pub fn not_found(message: String) -> Self {
        Self::new(404, message, "NOT_FOUND".to_string())
    }
    
    /// 创建超时错误
    pub fn timeout(message: String) -> Self {
        Self::new(408, message, "TIMEOUT".to_string())
    }
    
    /// 创建冲突错误
    pub fn conflict(message: String) -> Self {
        Self::new(409, message, "CONFLICT".to_string())
    }
    
    /// 创建验证错误
    pub fn validation_error(message: String) -> Self {
        Self::new(422, message, "VALIDATION_ERROR".to_string())
    }
    
    /// 创建服务不可用错误
    pub fn service_unavailable(message: String) -> Self {
        Self::new(503, message, "SERVICE_UNAVAILABLE".to_string())
    }
    
    /// 添加错误详情
    pub fn with_detail<T: Serialize>(mut self, key: String, value: T) -> Result<Self, serde_json::Error> {
        self.details.insert(key, serde_json::to_value(value)?);
        Ok(self)
    }
    
    /// 添加堆栈跟踪
    pub fn with_stack_trace(mut self, stack_trace: String) -> Self {
        self.stack_trace = Some(stack_trace);
        self
    }
}

/// 服务请求处理器trait
#[async_trait::async_trait]
pub trait ServiceHandler: Send + Sync {
    /// 处理服务请求
    async fn handle_request(&self, request: ServiceRequest) -> ServiceResponse;
    
    /// 获取处理器名称
    fn handler_name(&self) -> &str;
    
    /// 获取支持的方法列表
    fn supported_methods(&self) -> Vec<&str>;
    
    /// 验证请求
    fn validate_request(&self, request: &ServiceRequest) -> Result<(), ServiceError> {
        // 默认验证逻辑
        if request.method.is_empty() {
            return Err(ServiceError::validation_error("方法名不能为空".to_string()));
        }
        
        if !self.supported_methods().contains(&request.method.as_str()) {
            return Err(ServiceError::validation_error(format!("不支持的方法: {}", request.method)));
        }
        
        Ok(())
    }
}

/// 服务请求构建器
pub struct ServiceRequestBuilder {
    request: ServiceRequest,
}

impl ServiceRequestBuilder {
    /// 创建新的请求构建器
    pub fn new(service_name: String, method: String, source_service: String) -> Self {
        Self {
            request: ServiceRequest::new(service_name, method, source_service),
        }
    }
    
    /// 设置请求数据
    pub fn data<T: Serialize>(mut self, data: T) -> Result<Self, serde_json::Error> {
        self.request = self.request.with_data(data)?;
        Ok(self)
    }
    
    /// 设置请求头
    pub fn header(mut self, key: String, value: String) -> Self {
        self.request = self.request.with_header(key, value);
        self
    }
    
    /// 设置超时时间
    pub fn timeout(mut self, timeout_seconds: u64) -> Self {
        self.request = self.request.with_timeout(timeout_seconds);
        self
    }
    
    /// 设置重试次数
    pub fn retry(mut self, retry_count: u32) -> Self {
        self.request = self.request.with_retry(retry_count);
        self
    }
    
    /// 构建请求
    pub fn build(self) -> ServiceRequest {
        self.request
    }
}

/// 服务响应构建器
pub struct ServiceResponseBuilder {
    response: ServiceResponse,
}

impl ServiceResponseBuilder {
    /// 创建成功响应构建器
    pub fn success(request_id: String, service_name: String) -> Self {
        Self {
            response: ServiceResponse::success(request_id, service_name),
        }
    }
    
    /// 创建错误响应构建器
    pub fn error(request_id: String, service_name: String, error: ServiceError) -> Self {
        Self {
            response: ServiceResponse::error(request_id, service_name, error),
        }
    }
    
    /// 设置响应数据
    pub fn data<T: Serialize>(mut self, data: T) -> Result<Self, serde_json::Error> {
        self.response = self.response.with_data(data)?;
        Ok(self)
    }
    
    /// 设置响应头
    pub fn header(mut self, key: String, value: String) -> Self {
        self.response = self.response.with_header(key, value);
        self
    }
    
    /// 设置处理时间
    pub fn processing_time(mut self, processing_time: u64) -> Self {
        self.response = self.response.with_processing_time(processing_time);
        self
    }
    
    /// 构建响应
    pub fn build(self) -> ServiceResponse {
        self.response
    }
}

/// 常用的服务方法定义
pub mod methods {
    // 用户服务方法
    pub const USER_GET_PROFILE: &str = "get_profile";
    pub const USER_UPDATE_PROFILE: &str = "update_profile";
    pub const USER_GET_STATUS: &str = "get_status";
    pub const USER_UPDATE_STATUS: &str = "update_status";
    
    // 好友服务方法
    pub const FRIEND_SEND_REQUEST: &str = "send_message_request";
    pub const FRIEND_ACCEPT_REQUEST: &str = "accept_request";
    pub const FRIEND_REJECT_REQUEST: &str = "reject_request";
    pub const FRIEND_REMOVE_FRIEND: &str = "remove_friend";
    pub const FRIEND_GET_LIST: &str = "get_list";
    pub const FRIEND_GET_REQUESTS: &str = "get_requests";
    pub const FRIEND_BLOCK_USER: &str = "block_user";
    pub const FRIEND_UNBLOCK_USER: &str = "unblock_user";
    
    // 群组服务方法
    pub const GROUP_CREATE: &str = "create";
    pub const GROUP_JOIN: &str = "join";
    pub const GROUP_LEAVE: &str = "leave";
    pub const GROUP_INVITE: &str = "invite";
    pub const GROUP_KICK: &str = "kick";
    pub const GROUP_GET_INFO: &str = "get_info";
    pub const GROUP_UPDATE_INFO: &str = "update_info";
    pub const GROUP_GET_MEMBERS: &str = "get_members";
    pub const GROUP_SET_ADMIN: &str = "set_admin";
    pub const GROUP_REMOVE_ADMIN: &str = "remove_admin";
    
    // 推送服务方法
    pub const PUSH_SEND_MESSAGE: &str = "send_message";
    pub const PUSH_SEND_BATCH: &str = "send_batch";
    pub const PUSH_REGISTER_DEVICE: &str = "register_device";
    pub const PUSH_UNREGISTER_DEVICE: &str = "unregister_device";
    pub const PUSH_GET_STATS: &str = "get_stats";
    
    // 通用方法
    pub const HEALTH_CHECK: &str = "health_check";
    pub const GET_METRICS: &str = "get_metrics";
    pub const GET_INFO: &str = "get_info";
}

/// 服务名称定义
pub mod services {
    pub const USER_SERVICE: &str = "user-service";
    pub const FRIEND_SERVICE: &str = "friend-service";
    pub const GROUP_SERVICE: &str = "group-service";
    pub const PUSH_SERVICE: &str = "push-service";
    pub const MESSAGE_SERVICE: &str = "message-service";
    pub const NOTIFICATION_SERVICE: &str = "notification-service";
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_service_request_creation() {
        let request = ServiceRequest::new(
            "test-service".to_string(),
            "test-method".to_string(),
            "client-service".to_string(),
        );
        
        assert_eq!(request.service_name, "test-service");
        assert_eq!(request.method, "test-method");
        assert_eq!(request.source_service, "client-service");
        assert!(!request.request_id.is_empty());
    }
    
    #[test]
    fn test_service_response_creation() {
        let response = ServiceResponse::success("test-id".to_string(), "test-service".to_string());
        
        assert_eq!(response.request_id, "test-id");
        assert_eq!(response.service_name, "test-service");
        assert_eq!(response.status_code, 200);
        assert!(response.is_success());
    }
    
    #[test]
    fn test_service_error_creation() {
        let error = ServiceError::validation_error("测试错误".to_string());
        
        assert_eq!(error.code, 422);
        assert_eq!(error.message, "测试错误");
        assert_eq!(error.error_type, "VALIDATION_ERROR");
    }
    
    #[test]
    fn test_request_serialization() {
        let request = ServiceRequest::new(
            "test-service".to_string(),
            "test-method".to_string(),
            "client-service".to_string(),
        );
        
        let bytes = request.to_bytes().unwrap();
        let deserialized = ServiceRequest::from_bytes(&bytes).unwrap();
        
        assert_eq!(request.request_id, deserialized.request_id);
        assert_eq!(request.service_name, deserialized.service_name);
        assert_eq!(request.method, deserialized.method);
    }
} 