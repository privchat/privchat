use std::sync::Arc;
use tokio::time::Duration;
use tracing::{info, error, debug, warn};
use uuid::Uuid;
use crate::domain::events::DomainEvent;
use crate::infra::event_bus::EventBus;
use crate::infra::redis::RedisClient;
use crate::push::types::{PushIntent, PushPayload};
use crate::push::intent_state::IntentStateManager;
use crate::error::Result;

/// Push Planner（推送规划器）
/// 
/// 职责：
/// - 监听 DomainEvent::MessageCommitted 事件
/// - 检查用户是否在线（查询 Redis Presence）
/// - 如果离线，生成 PushIntent 并发送到 Worker
/// - 处理 MessageRevoked 和 UserOnline 事件（Phase 3）
pub struct PushPlanner {
    redis: Option<Arc<RedisClient>>,
    intent_state: Arc<IntentStateManager>,  // Phase 3: 共享状态管理器
}

impl PushPlanner {
    pub fn new() -> Self {
        Self {
            redis: None,
            intent_state: Arc::new(IntentStateManager::new()),
        }
    }
    
    /// 创建带 Redis 的 Planner（Phase 2）
    pub fn with_redis(redis: Arc<RedisClient>) -> Self {
        Self {
            redis: Some(redis),
            intent_state: Arc::new(IntentStateManager::new()),
        }
    }
    
    /// 创建带共享状态管理器的 Planner（Phase 3）
    pub fn with_state_manager(redis: Option<Arc<RedisClient>>, intent_state: Arc<IntentStateManager>) -> Self {
        Self {
            redis,
            intent_state,
        }
    }
    
    /// 获取 Intent 状态管理器（供 Worker 使用）
    pub fn intent_state(&self) -> Arc<IntentStateManager> {
        Arc::clone(&self.intent_state)
    }
    
    /// 启动 Planner，监听事件
    pub async fn start(&self, event_bus: Arc<EventBus>, sender: tokio::sync::mpsc::Sender<PushIntent>) -> Result<()> {
        let mut receiver = event_bus.subscribe();
        
        info!("[PUSH PLANNER] Started");
        
        loop {
            match receiver.recv().await {
                Ok(event @ DomainEvent::MessageCommitted { .. }) => {
                    if let Err(e) = self.handle_message_committed(event, &sender).await {
                        error!("[PUSH PLANNER] Failed to handle MessageCommitted: {}", e);
                    }
                }
                Ok(event @ DomainEvent::MessageRevoked { .. }) => {
                    if let Err(e) = self.handle_message_revoked(event).await {
                        error!("[PUSH PLANNER] Failed to handle MessageRevoked: {}", e);
                    }
                }
                Ok(event @ DomainEvent::UserOnline { .. }) => {
                    if let Err(e) = self.handle_user_online(event).await {
                        error!("[PUSH PLANNER] Failed to handle UserOnline: {}", e);
                    }
                }
                Ok(event @ DomainEvent::MessageDelivered { .. }) => {  // ✨ Phase 3.5
                    if let Err(e) = self.handle_message_delivered(event).await {
                        error!("[PUSH PLANNER] Failed to handle MessageDelivered: {}", e);
                    }
                }
                Ok(event @ DomainEvent::DeviceOnline { .. }) => {  // ✨ Phase 3.5
                    if let Err(e) = self.handle_device_online(event).await {
                        error!("[PUSH PLANNER] Failed to handle DeviceOnline: {}", e);
                    }
                }
                Err(e) => {
                    error!("[PUSH PLANNER] Event bus error: {}", e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }
    
    async fn handle_message_committed(
        &self,
        event: DomainEvent,
        sender: &tokio::sync::mpsc::Sender<PushIntent>,
    ) -> Result<()> {
        // 从 DomainEvent 中提取字段
        let (message_id, conversation_id, sender_id, recipient_id, content_preview, timestamp, device_id) = match event {
            DomainEvent::MessageCommitted {
                message_id,
                conversation_id,
                sender_id,
                recipient_id,
                content_preview,
                timestamp,
                device_id,  // ✨ Phase 3.5: 可选的设备ID
                ..
            } => (message_id, conversation_id, sender_id, recipient_id, content_preview, timestamp, device_id),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_committed");
                return Ok(());
            }
        };
        
        info!(
            "[PUSH PLANNER] Received MessageCommitted: message_id={}, recipient_id={}, sender_id={}, device_id={:?}",
            message_id, recipient_id, sender_id, device_id
        );
        
        // ✨ Phase 3.5: 如果指定了 device_id，只为该设备生成 Intent
        if let Some(device_id) = device_id {
            let intent_id = Uuid::new_v4().to_string();
            let intent = PushIntent::new(
                intent_id.clone(),
                message_id,
                conversation_id,
                recipient_id,
                device_id.clone(),  // ✨ 设备级 Intent
                sender_id,
                PushPayload {
                    r#type: "new_message".to_string(),
                    conversation_id,
                    message_id,
                    sender_id,
                    content_preview,
                },
                timestamp,
            );
            
            // 注册设备级 Intent
            self.intent_state.register_device_intent(&intent_id, message_id, recipient_id, &device_id).await;
            
            // 发送到 Worker 队列
            sender.send(intent.clone()).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Failed to send intent to worker: {}", e))
            })?;
            
            debug!("[PUSH PLANNER] Device-level Intent sent to worker: intent_id={}, device_id={}", intent.intent_id, device_id);
            return Ok(());
        }
        
        // 兼容旧逻辑：检查用户是否在线（查询 Redis Presence）
        let is_online = self.check_user_online(recipient_id).await?;
        
        if is_online {
            debug!(
                "[PUSH PLANNER] User {} is online, skip push",
                recipient_id
            );
            return Ok(());
        }
        
        debug!(
            "[PUSH PLANNER] User {} is offline, generating push intent (legacy mode)",
            recipient_id
        );
        
        // 生成用户级 PushIntent（兼容旧逻辑）
        let intent_id = Uuid::new_v4().to_string();
        let intent = PushIntent::new(
            intent_id.clone(),
            message_id,
            conversation_id,
            recipient_id,
            "".to_string(),  // 旧逻辑：device_id 为空
            sender_id,
            PushPayload {
                r#type: "new_message".to_string(),
                conversation_id,
                message_id,
                sender_id,
                content_preview,
            },
            timestamp,
        );
        
        // 注册 Intent（兼容旧逻辑）
        self.intent_state.register_intent(&intent_id, message_id, recipient_id).await;
        
        // 发送到 Worker 队列
        sender.send(intent.clone()).await.map_err(|e| {
            crate::error::ServerError::Internal(format!("Failed to send intent to worker: {}", e))
        })?;
        
        debug!("[PUSH PLANNER] Intent sent to worker: intent_id={}", intent.intent_id);
        
        Ok(())
    }
    
    /// 检查用户是否在线（查询 Redis Presence）
    /// 
    /// 查询逻辑：
    /// - 查找 presence:{user_id}:* 的所有 key
    /// - 如果存在任何 key，说明用户有在线设备
    async fn check_user_online(&self, user_id: u64) -> Result<bool> {
        let redis = match &self.redis {
            Some(r) => r,
            None => {
                // 如果没有 Redis，默认返回 false（离线），需要推送
                warn!("[PUSH PLANNER] Redis not configured, assuming user offline");
                return Ok(false);
            }
        };
        
        // 查找该用户的所有设备 presence key
        let pattern = format!("presence:{}:*", user_id);
        let keys = redis.keys(&pattern).await?;
        
        // 如果存在任何 key，说明用户有在线设备
        if !keys.is_empty() {
            debug!(
                "[PUSH PLANNER] User {} has {} online device(s)",
                user_id, keys.len()
            );
            return Ok(true);
        }
        
        // 没有找到 presence key，用户离线
        Ok(false)
    }
    
    /// 处理消息撤销事件（Phase 3）
    async fn handle_message_revoked(&self, event: DomainEvent) -> Result<()> {
        let message_id = match event {
            DomainEvent::MessageRevoked {
                message_id,
                ..
            } => message_id,
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_revoked");
                return Ok(());
            }
        };
        
        info!("[PUSH PLANNER] Received MessageRevoked: message_id={}", message_id);
        
        // 标记 Intent 为 revoked（Phase 3.5: 返回取消的数量）
        let count = self.intent_state.mark_revoked(message_id).await;
        if count > 0 {
            info!("[PUSH PLANNER] {} intent(s) marked as revoked for message {}", count, message_id);
        } else {
            debug!("[PUSH PLANNER] No intent found for message {}", message_id);
        }
        
        Ok(())
    }
    
    /// 处理用户上线事件（Phase 3，兼容旧逻辑）
    async fn handle_user_online(&self, event: DomainEvent) -> Result<()> {
        let user_id = match event {
            DomainEvent::UserOnline {
                user_id,
                ..
            } => user_id,
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_user_online");
                return Ok(());
            }
        };
        
        info!("[PUSH PLANNER] Received UserOnline: user_id={}", user_id);
        
        // 标记用户的所有待推送 Intent 为 cancelled
        let count = self.intent_state.mark_cancelled(user_id).await;
        if count > 0 {
            info!("[PUSH PLANNER] {} intent(s) marked as cancelled for user {}", count, user_id);
        }
        
        Ok(())
    }
    
    /// ✨ Phase 3.5: 处理消息已送达事件
    async fn handle_message_delivered(&self, event: DomainEvent) -> Result<()> {
        let (message_id, device_id) = match event {
            DomainEvent::MessageDelivered {
                message_id,
                device_id,
                ..
            } => (message_id, device_id),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_delivered");
                return Ok(());
            }
        };
        
        info!("[PUSH PLANNER] Received MessageDelivered: message_id={}, device_id={}", message_id, device_id);
        
        // 取消该设备对应的 Push Intent
        let count = self.intent_state.mark_cancelled_by_device(&device_id, Some(message_id)).await;
        if count > 0 {
            info!("[PUSH PLANNER] {} intent(s) cancelled for device {} (message {})", count, device_id, message_id);
        }
        
        Ok(())
    }
    
    /// ✨ Phase 3.5: 处理设备上线事件
    async fn handle_device_online(&self, event: DomainEvent) -> Result<()> {
        let device_id = match event {
            DomainEvent::DeviceOnline {
                device_id,
                ..
            } => device_id,
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_device_online");
                return Ok(());
            }
        };
        
        info!("[PUSH PLANNER] Received DeviceOnline: device_id={}", device_id);
        
        // 取消该设备的所有待推送 Intent
        let count = self.intent_state.mark_cancelled_by_device(&device_id, None).await;
        if count > 0 {
            info!("[PUSH PLANNER] {} intent(s) cancelled for device {}", count, device_id);
        }
        
        Ok(())
    }
}
