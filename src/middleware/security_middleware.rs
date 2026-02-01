/// 安全中间件
/// 
/// 集成到消息处理链路，自动执行安全检查

use std::sync::Arc;
use tracing::{info, warn};

use crate::error::{Result, ServerError};
use crate::security::{SecurityService, SecurityCheckResult};

/// 安全中间件
pub struct SecurityMiddleware {
    security_service: Arc<SecurityService>,
}

impl SecurityMiddleware {
    pub fn new(security_service: Arc<SecurityService>) -> Self {
        Self { security_service }
    }
    
    /// 连接建立时的安全检查
    pub async fn check_connection(&self, peer_ip: &str) -> Result<()> {
        let result = self.security_service.check_connection(peer_ip).await;
        
        if !result.allowed {
            warn!("连接被拒绝: {} - {}", peer_ip, result.reason.unwrap_or_default());
            return Err(ServerError::RateLimit("Connection denied".into()));
        }
        
        Ok(())
    }
    
    /// RPC 调用前的安全检查
    pub async fn check_rpc(
        &self,
        user_id: u64,
        device_id: &str,
        rpc_method: &str,
        channel_id: Option<u64>,
    ) -> Result<SecurityCheckResult> {
        let result = self.security_service
            .check_rpc(user_id, device_id, rpc_method, channel_id)
            .await;
        
        // 如果需要 shadow ban，假装成功但不执行
        if result.should_silent_drop {
            info!("用户 {} 被 shadow ban，RPC {} 将被静默丢弃", user_id, rpc_method);
            return Ok(result);
        }
        
        // 如果需要 throttle，延迟处理
        if let Some(delay) = result.throttle_delay {
            tokio::time::sleep(delay).await;
        }
        
        if !result.allowed {
            return Err(ServerError::RateLimit(
                result.reason.unwrap_or_else(|| "Rate limit exceeded".to_string())
            ));
        }
        
        Ok(result)
    }
    
    /// 消息发送前的安全检查
    pub async fn check_send_message(
        &self,
        user_id: u64,
        device_id: &str,
        channel_id: u64,
        recipient_count: usize,
        message_size: usize,
        is_media: bool,
    ) -> Result<SecurityCheckResult> {
        let result = self.security_service
            .check_send_message(
                user_id,
                device_id,
                channel_id,
                recipient_count,
                message_size,
                is_media,
            )
            .await;
        
        // Shadow ban：假装成功，但不会真正投递消息
        if result.should_silent_drop {
            info!(
                "用户 {} 被 shadow ban，消息将被静默丢弃（会话: {}, 接收者: {}）",
                user_id, channel_id, recipient_count
            );
            return Ok(result);
        }
        
        // Throttle：延迟处理
        if let Some(delay) = result.throttle_delay {
            tokio::time::sleep(delay).await;
        }
        
        if !result.allowed {
            return Err(ServerError::RateLimit(
                result.reason.unwrap_or_else(|| "Message rate limit exceeded".to_string())
            ));
        }
        
        Ok(result)
    }
    
    /// 认证失败时记录
    pub async fn record_auth_failure(&self, user_id: u64, device_id: &str, ip: &str) {
        self.security_service.record_auth_failure(user_id, device_id, ip).await;
    }
    
    /// 奖励良好行为
    pub async fn reward_good_behavior(&self, user_id: u64, device_id: &str) {
        self.security_service.reward_good_behavior(user_id, device_id).await;
    }
}
