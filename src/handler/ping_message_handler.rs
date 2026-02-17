use crate::context::RequestContext;
use crate::handler::MessageHandler;
use crate::Result;
use async_trait::async_trait;
use tracing::debug;

/// Ping消息处理器
pub struct PingMessageHandler;

impl PingMessageHandler {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl MessageHandler for PingMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        debug!(
            "🏓 PingMessageHandler: 处理来自会话 {} 的Ping请求",
            context.session_id
        );

        // 解析Ping请求
        let _ping_request: privchat_protocol::protocol::PingRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码Ping请求失败: {}", e))
            })?;

        // 创建Pong响应
        let pong_response = privchat_protocol::protocol::PongResponse {
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64,
        };

        // 编码响应
        let response_bytes = privchat_protocol::encode_message(&pong_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码Pong响应失败: {}", e)))?;

        debug!("✅ PingMessageHandler: Ping请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "PingMessageHandler"
    }
}
