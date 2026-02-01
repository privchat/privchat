//! 通知服务：向客户端推送消息等与用户联系的能力
//!
//! 封装「按会话发送 PushMessageRequest」等能力，保证客户端能按 biz_type=7 接收并落库，
//! UI 通过 get_channels / get_messages 从 SQLite 读取。
//! 未来可扩展：其他联系用户的功能（如系统通知、提醒等）均可通过本服务下发。

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;
use msgtrans::{SessionId, packet::Packet};
use privchat_protocol::message::{PushMessageRequest, MessageType};

use crate::error::{Result, ServerError};

/// 通知服务：向指定会话/用户推送消息等
pub struct NotificationService {
    /// 传输层（运行时由 server 设置）
    transport: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
}

impl NotificationService {
    pub fn new() -> Self {
        Self {
            transport: Arc::new(RwLock::new(None)),
        }
    }

    /// 设置传输层（由 server 在启动时调用）
    pub async fn set_transport(&self, transport: Arc<msgtrans::transport::TransportServer>) {
        *self.transport.write().await = Some(transport);
    }

    /// 向指定会话发送一条 PushMessageRequest（biz_type=7），客户端可落库并在 UI 中展示。
    pub async fn send_push_to_session(
        &self,
        session_id: &SessionId,
        message: &PushMessageRequest,
    ) -> Result<()> {
        let guard = self.transport.read().await;
        let transport = guard
            .as_ref()
            .ok_or_else(|| ServerError::Internal("TransportServer 未初始化".to_string()))?;

        let recv_data = privchat_protocol::encode_message(message)
            .map_err(|e| ServerError::Protocol(format!("编码 PushMessageRequest 失败: {}", e)))?;

        let message_id = 0u32;
        let mut packet = Packet::one_way(message_id, recv_data);
        packet.set_biz_type(MessageType::PushMessageRequest as u8);

        transport
            .send_to_session(session_id.clone(), packet)
            .await
            .map_err(|e| ServerError::Internal(format!("发送 PushMessageRequest 失败: {}", e)))?;

        info!(
            "✅ NotificationService: 已向会话 {} 发送 PushMessageRequest (server_message_id={}, channel_id={})",
            session_id, message.server_message_id, message.channel_id
        );
        Ok(())
    }
}
