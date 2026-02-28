use crate::context::RequestContext;
use crate::handler::MessageHandler;
use crate::infra::SubscribeManager;
use crate::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{info, warn};

pub struct DisconnectMessageHandler {
    connection_manager: Arc<crate::infra::ConnectionManager>,
    subscribe_manager: Arc<SubscribeManager>,
}

impl DisconnectMessageHandler {
    pub fn new(
        connection_manager: Arc<crate::infra::ConnectionManager>,
        subscribe_manager: Arc<SubscribeManager>,
    ) -> Self {
        Self {
            connection_manager,
            subscribe_manager,
        }
    }
}

#[async_trait]
impl MessageHandler for DisconnectMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!(
            "🔌 DisconnectMessageHandler: 处理来自会话 {} 的断开连接请求",
            context.session_id
        );

        // 解析断开连接请求
        let disconnect_request: privchat_protocol::protocol::DisconnectRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码断开连接请求失败: {}", e))
            })?;

        info!(
            "🔌 DisconnectMessageHandler: 用户请求断开连接，原因: {:?}",
            disconnect_request.reason
        );

        // 注销连接
        if let Err(e) = self
            .connection_manager
            .unregister_connection(context.session_id.clone())
            .await
        {
            warn!("⚠️ DisconnectMessageHandler: 注销连接失败: {}", e);
        } else {
            info!("✅ DisconnectMessageHandler: 连接已注销");
        }

        // 清理频道订阅
        let left_channels = self.subscribe_manager.on_session_disconnect(&context.session_id);
        if !left_channels.is_empty() {
            info!(
                "🔌 DisconnectMessageHandler: Session {} 离开频道 {:?}",
                context.session_id, left_channels
            );
        }

        // 创建断开连接响应
        let disconnect_response =
            privchat_protocol::protocol::DisconnectResponse { acknowledged: true };

        // 编码响应
        let response_bytes =
            privchat_protocol::encode_message(&disconnect_response).map_err(|e| {
                crate::error::ServerError::Protocol(format!("编码断开连接响应失败: {}", e))
            })?;

        info!("✅ DisconnectMessageHandler: 断开连接请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "DisconnectMessageHandler"
    }
}
