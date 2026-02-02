use async_trait::async_trait;
use crate::handler::MessageHandler;
use crate::context::RequestContext;
use crate::Result;
use tracing::info;

/// 订阅消息处理器
pub struct SubscribeMessageHandler;

impl SubscribeMessageHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MessageHandler for SubscribeMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!("📡 SubscribeMessageHandler: 处理来自会话 {} 的订阅请求", context.session_id);
        
        // 解析订阅请求
        let subscribe_request: privchat_protocol::protocol::SubscribeRequest = privchat_protocol::decode_message(&context.data)
            .map_err(|e| crate::error::ServerError::Protocol(format!("解码订阅请求失败: {}", e)))?;
        
        info!("📡 SubscribeMessageHandler: 请求订阅频道: {}", subscribe_request.channel_id);
        
        // 创建订阅响应
        let subscribe_response = privchat_protocol::protocol::SubscribeResponse {
            local_message_id: subscribe_request.local_message_id,
            channel_id: subscribe_request.channel_id,
            channel_type: subscribe_request.channel_type,
            action: subscribe_request.action,
            reason_code: 0, // 成功
        };
        
        // 编码响应
        let response_bytes = privchat_protocol::encode_message(&subscribe_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码订阅响应失败: {}", e)))?;
        
        info!("✅ SubscribeMessageHandler: 订阅请求处理完成");
        
        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "SubscribeMessageHandler"
    }
} 