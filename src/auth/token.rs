use crate::error::ServerError;
use tracing::{debug, warn};

/// 简化的令牌认证器
/// 在实际项目中，这里会连接到认证服务或数据库
pub struct TokenAuth;

impl TokenAuth {
    /// 创建新的令牌认证器
    pub fn new() -> Self {
        Self
    }
    
    /// 验证令牌并返回用户ID
    /// 当前实现：简单的前缀解析，用于演示目的
    /// 令牌格式: "user_<user_id>_token" 例如 "user_123_token"
    pub async fn verify_token(&self, token: &str) -> Result<String, ServerError> {
        debug!("验证令牌: {}", token);
        
        // 简化验证逻辑
        if token.starts_with("user_") && token.ends_with("_token") {
            // 提取用户ID
            let parts: Vec<&str> = token.split('_').collect();
            if parts.len() == 3 {
                let user_id = parts[1].to_string();
                if !user_id.is_empty() {
                    debug!("令牌验证成功: user_id={}", user_id);
                    return Ok(user_id);
                }
            }
        }
        
        warn!("令牌验证失败: {}", token);
        Err(ServerError::InvalidToken)
    }
    
    /// 验证设备ID格式
    pub fn validate_device_id(&self, device_id: &str) -> Result<(), ServerError> {
        if device_id.is_empty() {
            return Err(ServerError::InvalidDeviceId);
        }
        
        // 简单的设备ID格式验证
        if device_id.len() < 3 || device_id.len() > 64 {
            return Err(ServerError::InvalidDeviceId);
        }
        
        Ok(())
    }
    
    /// 验证设备类型
    pub fn validate_device_type(&self, device_type: &str) -> Result<(), ServerError> {
        match device_type {
            "ios" | "android" | "web" | "desktop" | "other" => Ok(()),
            _ => Err(ServerError::InvalidDeviceType),
        }
    }
}

impl Default for TokenAuth {
    fn default() -> Self {
        Self::new()
    }
} 