use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, error};
use serde::{Serialize, Deserialize};
use rustc_hash::FxHashMap;

use crate::error::ServerError;
use super::message::{OfflineMessage, MessagePriority, DeliveryStatus};
use super::storage::{StorageBackend, MemoryStorage};

/// 队列配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueConfig {
    /// 每个用户最大离线消息数
    pub max_messages_per_user: usize,
    /// 消息默认过期时间（天）
    pub default_expiry_days: i64,
    /// 队列持久化间隔（秒）
    pub persistence_interval_secs: u64,
    /// 投递重试间隔（秒）
    pub delivery_retry_interval_secs: u64,
    /// 批量处理大小
    pub batch_size: usize,
    /// 是否启用持久化
    pub enable_persistence: bool,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            max_messages_per_user: 500,
            default_expiry_days: 7,
            persistence_interval_secs: 30,
            delivery_retry_interval_secs: 5,
            batch_size: 100,
            enable_persistence: true,
        }
    }
}

/// 队列统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueStats {
    /// 总消息数
    pub total_messages: u64,
    /// 待投递消息数
    pub pending_messages: u64,
    /// 投递中消息数
    pub delivering_messages: u64,
    /// 已投递消息数
    pub delivered_messages: u64,
    /// 失败消息数
    pub failed_messages: u64,
    /// 过期消息数
    pub expired_messages: u64,
    /// 用户数量
    pub user_count: u64,
    /// 队列内存使用（字节）
    pub memory_usage_bytes: u64,
    /// 平均消息大小（字节）
    pub avg_message_size_bytes: u64,
    /// 最大队列长度
    pub max_queue_length: usize,
    /// 平均队列长度
    pub avg_queue_length: f64,
}

/// 用户消息队列
#[derive(Debug)]
struct UserQueue {
    /// 消息队列（按优先级和时间排序）
    messages: BinaryHeap<OfflineMessage>,
    /// 消息ID映射（用于快速查找和删除）
    message_map: FxHashMap<u64, usize>,
    /// 队列大小限制
    max_size: usize,
    /// 总字节数
    total_bytes: u64,
}

impl UserQueue {
    fn new(max_size: usize) -> Self {
        Self {
            messages: BinaryHeap::new(),
            message_map: FxHashMap::default(),
            max_size,
            total_bytes: 0,
        }
    }

    /// 添加消息到队列
    fn push(&mut self, message: OfflineMessage) -> Result<Option<OfflineMessage>, ServerError> {
        let message_size = message.size_bytes() as u64;
        
        // 检查是否超过大小限制
        if self.messages.len() >= self.max_size {
            // 移除最旧的低优先级消息
            if let Some(removed) = self.remove_oldest_low_priority() {
                self.total_bytes -= removed.size_bytes() as u64;
            } else {
                return Err(ServerError::Internal("Queue is full and cannot remove old messages".to_string()));
            }
        }

        self.message_map.insert(message.message_id, self.messages.len());
        self.total_bytes += message_size;
        self.messages.push(message.clone());
        
        Ok(Some(message))
    }

    /// 获取所有准备投递的消息
    fn get_ready_messages(&self) -> Vec<OfflineMessage> {
        self.messages.iter()
            .filter(|msg| msg.is_ready_for_delivery())
            .cloned()
            .collect()
    }

    /// 获取指定数量的消息
    fn pop_messages(&mut self, limit: usize) -> Vec<OfflineMessage> {
        let mut messages = Vec::new();
        
        while messages.len() < limit && !self.messages.is_empty() {
            if let Some(message) = self.messages.pop() {
                if message.is_ready_for_delivery() {
                    self.message_map.remove(&message.message_id);
                    self.total_bytes -= message.size_bytes() as u64;
                    messages.push(message);
                } else {
                    // 消息还不能投递，重新放回队列
                    self.messages.push(message);
                    break;
                }
            }
        }
        
        messages
    }

    /// 更新消息状态
    fn update_message_status(&mut self, message_id: u64, status: DeliveryStatus) -> bool {
        // 由于 BinaryHeap 不支持直接修改，我们需要重建堆
        let mut messages: Vec<_> = self.messages.drain().collect();
        let mut found = false;
        
        for message in &mut messages {
            if message.message_id == message_id {
                message.status = status.clone();
                found = true;
                break;
            }
        }
        
        if found {
            self.messages = messages.into_iter().collect();
            self.rebuild_message_map();
        }
        
        found
    }

    /// 删除指定消息
    fn remove_message(&mut self, message_id: u64) -> Option<OfflineMessage> {
        let mut messages: Vec<_> = self.messages.drain().collect();
        let mut removed_message = None;
        
        messages.retain(|msg| {
            if msg.message_id == message_id {
                removed_message = Some(msg.clone());
                self.total_bytes -= msg.size_bytes() as u64;
                false
            } else {
                true
            }
        });
        
        if removed_message.is_some() {
            self.messages = messages.into_iter().collect();
            self.rebuild_message_map();
        }
        
        removed_message
    }

    /// 清理过期消息
    fn cleanup_expired(&mut self) -> usize {
        let original_len = self.messages.len();
        let mut messages: Vec<_> = self.messages.drain().collect();
        let mut removed_bytes = 0u64;
        
        messages.retain(|msg| {
            if msg.is_expired() {
                removed_bytes += msg.size_bytes() as u64;
                false
            } else {
                true
            }
        });
        
        self.total_bytes -= removed_bytes;
        self.messages = messages.into_iter().collect();
        self.rebuild_message_map();
        
        original_len - self.messages.len()
    }

    /// 移除最旧的低优先级消息
    fn remove_oldest_low_priority(&mut self) -> Option<OfflineMessage> {
        let mut messages: Vec<_> = self.messages.drain().collect();
        
        // 按优先级和时间排序，找到最旧的低优先级消息
        messages.sort_by(|a, b| {
            match a.priority.cmp(&b.priority) {
                std::cmp::Ordering::Equal => b.created_at.cmp(&a.created_at),
                other => other,
            }
        });
        
        // 移除最旧的低优先级消息
        if let Some(removed) = messages.pop() {
            self.messages = messages.into_iter().collect();
            self.rebuild_message_map();
            Some(removed)
        } else {
            None
        }
    }

    /// 重建消息ID映射
    fn rebuild_message_map(&mut self) {
        self.message_map.clear();
        for (index, message) in self.messages.iter().enumerate() {
            self.message_map.insert(message.message_id, index);
        }
    }

    /// 获取队列统计信息
    fn get_stats(&self) -> (usize, u64, usize, usize, usize, usize, usize) {
        let mut pending = 0;
        let mut delivering = 0;
        let mut delivered = 0;
        let mut failed = 0;
        let mut expired = 0;
        
        for message in &self.messages {
            match message.status {
                DeliveryStatus::Pending => pending += 1,
                DeliveryStatus::Delivering => delivering += 1,
                DeliveryStatus::Delivered => delivered += 1,
                DeliveryStatus::Failed => failed += 1,
                DeliveryStatus::Expired => expired += 1,
            }
        }
        
        (self.messages.len(), self.total_bytes, pending, delivering, delivered, failed, expired)
    }
}

/// 离线消息队列管理器
pub struct OfflineQueueManager<S: StorageBackend + 'static> {
    /// 用户队列映射
    user_queues: Arc<RwLock<FxHashMap<u64, UserQueue>>>,
    /// 存储后端
    storage: Arc<S>,
    /// 配置
    config: QueueConfig,
    /// 消息ID生成器
    message_id_counter: Arc<tokio::sync::Mutex<u64>>,
    /// 运行状态
    running: Arc<tokio::sync::Mutex<bool>>,
}

impl<S: StorageBackend + 'static> OfflineQueueManager<S> {
    /// 创建新的队列管理器
    pub async fn new(storage: S, config: QueueConfig) -> Result<Self, ServerError> {
        let manager = Self {
            user_queues: Arc::new(RwLock::new(FxHashMap::default())),
            storage: Arc::new(storage),
            config,
            message_id_counter: Arc::new(tokio::sync::Mutex::new(1)),
            running: Arc::new(tokio::sync::Mutex::new(false)),
        };
        
        // 从存储中恢复消息
        if manager.config.enable_persistence {
            manager.restore_from_storage().await?;
        }
        
        Ok(manager)
    }

    /// 获取存储后端引用
    pub fn storage(&self) -> &Arc<S> {
        &self.storage
    }

    /// 启动队列管理器
    pub async fn start(&self) -> Result<(), ServerError> {
        let mut running = self.running.lock().await;
        if *running {
            return Err(ServerError::Internal("Queue manager is already running".to_string()));
        }
        *running = true;
        
        info!("Starting offline queue manager");
        
        // 启动后台任务
        if self.config.enable_persistence {
            self.start_persistence_task().await;
        }
        
        self.start_cleanup_task().await;
        self.start_delivery_retry_task().await;
        
        Ok(())
    }

    /// 停止队列管理器
    pub async fn stop(&self) -> Result<(), ServerError> {
        let mut running = self.running.lock().await;
        if !*running {
            return Ok(());
        }
        *running = false;
        
        info!("Stopping offline queue manager");
        
        // 最后一次持久化
        if self.config.enable_persistence {
            self.persist_to_storage().await?;
        }
        
        Ok(())
    }

    /// 添加离线消息
    pub async fn add_message(
        &self,
        user_id: u64,
        sender_id: String,
        channel_id: String,
        payload: bytes::Bytes,
        message_type: String,
        priority: Option<MessagePriority>,
    ) -> Result<u64, ServerError> {
        let message_id = self.generate_message_id().await;
        
        let mut message = OfflineMessage::new(
            message_id,
            user_id.to_string(),
            sender_id,
            channel_id,
            payload,
            message_type,
        );
        
        if let Some(priority) = priority {
            message.priority = priority;
        }
        
        // 添加到内存队列
        {
            let mut queues = self.user_queues.write().await;
            let queue = queues.entry(user_id)
                .or_insert_with(|| UserQueue::new(self.config.max_messages_per_user));
            
            queue.push(message.clone())?;
        }
        
        // 持久化到存储
        if self.config.enable_persistence {
            self.storage.store_message(&message).await?;
        }
        
        debug!("Added offline message {} for user {}", message_id, user_id);
        Ok(message_id)
    }

    /// 获取用户的离线消息（用于投递）
    pub async fn get_messages_for_delivery(&self, user_id: u64, limit: Option<usize>) -> Result<Vec<OfflineMessage>, ServerError> {
        let limit = limit.unwrap_or(self.config.batch_size);
        
        let mut queues = self.user_queues.write().await;
        if let Some(queue) = queues.get_mut(&user_id) {
            let messages = queue.pop_messages(limit);
            debug!("Retrieved {} messages for delivery to user {}", messages.len(), user_id);
            Ok(messages)
        } else {
            Ok(Vec::new())
        }
    }

    /// 标记消息投递成功
    pub async fn mark_delivered(&self, user_id: u64, message_ids: &[u64]) -> Result<usize, ServerError> {
        let mut marked_count = 0;
        
        {
            let mut queues = self.user_queues.write().await;
            if let Some(queue) = queues.get_mut(&user_id) {
                for &message_id in message_ids {
                    if queue.update_message_status(message_id, DeliveryStatus::Delivered) {
                        marked_count += 1;
                    }
                }
            }
        }
        
        // 从持久化存储中删除
        if self.config.enable_persistence && marked_count > 0 {
            self.storage.delete_messages(user_id, message_ids).await?;
        }
        
        debug!("Marked {} messages as delivered for user {}", marked_count, user_id);
        Ok(marked_count)
    }

    /// 标记消息投递失败
    pub async fn mark_failed(&self, user_id: u64, message_id: u64) -> Result<bool, ServerError> {
        let mut queues = self.user_queues.write().await;
        if let Some(queue) = queues.get_mut(&user_id) {
            // 这里需要更复杂的逻辑来处理重试
            let updated = queue.update_message_status(message_id, DeliveryStatus::Failed);
            debug!("Marked message {} as failed for user {}", message_id, user_id);
            Ok(updated)
        } else {
            Ok(false)
        }
    }

    /// 清理用户的所有离线消息
    pub async fn clear_user_messages(&self, user_id: u64) -> Result<usize, ServerError> {
        let removed_count = {
            let mut queues = self.user_queues.write().await;
            if let Some(queue) = queues.remove(&user_id) {
                queue.messages.len()
            } else {
                0
            }
        };
        
        // 从持久化存储中删除
        if self.config.enable_persistence && removed_count > 0 {
            self.storage.delete_all_messages(user_id).await?;
        }
        
        debug!("Cleared {} messages for user {}", removed_count, user_id);
        Ok(removed_count)
    }

    /// 获取队列统计信息
    pub async fn get_stats(&self) -> Result<QueueStats, ServerError> {
        let queues = self.user_queues.read().await;
        let mut stats = QueueStats {
            total_messages: 0,
            pending_messages: 0,
            delivering_messages: 0,
            delivered_messages: 0,
            failed_messages: 0,
            expired_messages: 0,
            user_count: queues.len() as u64,
            memory_usage_bytes: 0,
            avg_message_size_bytes: 0,
            max_queue_length: 0,
            avg_queue_length: 0.0,
        };
        
        let mut total_queue_length = 0;
        
        for queue in queues.values() {
            let (len, bytes, pending, delivering, delivered, failed, expired) = queue.get_stats();
            
            stats.total_messages += len as u64;
            stats.pending_messages += pending as u64;
            stats.delivering_messages += delivering as u64;
            stats.delivered_messages += delivered as u64;
            stats.failed_messages += failed as u64;
            stats.expired_messages += expired as u64;
            stats.memory_usage_bytes += bytes;
            
            if len > stats.max_queue_length {
                stats.max_queue_length = len;
            }
            
            total_queue_length += len;
        }
        
        if stats.user_count > 0 {
            stats.avg_queue_length = total_queue_length as f64 / stats.user_count as f64;
        }
        
        if stats.total_messages > 0 {
            stats.avg_message_size_bytes = stats.memory_usage_bytes / stats.total_messages;
        }
        
        Ok(stats)
    }

    /// 生成消息ID
    async fn generate_message_id(&self) -> u64 {
        let mut counter = self.message_id_counter.lock().await;
        *counter += 1;
        *counter
    }

    /// 从存储中恢复消息
    async fn restore_from_storage(&self) -> Result<(), ServerError> {
        info!("Restoring messages from storage");
        
        // 这里需要实现从存储中恢复所有用户的消息
        // 由于存储后端的限制，我们可能需要维护一个用户列表
        // 暂时跳过这个实现，在实际使用中可以通过其他方式获取用户列表
        
        Ok(())
    }

    /// 持久化到存储
    async fn persist_to_storage(&self) -> Result<(), ServerError> {
        debug!("Persisting messages to storage");
        
        let queues = self.user_queues.read().await;
        for (_user_id, queue) in queues.iter() {
            for message in &queue.messages {
                self.storage.store_message(message).await?;
            }
        }
        
        Ok(())
    }

    /// 启动持久化任务
    async fn start_persistence_task(&self) {
        let storage = self.storage.clone();
        let queues = self.user_queues.clone();
        let running = self.running.clone();
        let interval = self.config.persistence_interval_secs;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval));
            
            loop {
                interval.tick().await;
                
                {
                    let running = running.lock().await;
                    if !*running {
                        break;
                    }
                }
                
                // 持久化消息
                let queues = queues.read().await;
                for (user_id, queue) in queues.iter() {
                    for message in &queue.messages {
                        if let Err(e) = storage.store_message(message).await {
                            error!("Failed to persist message {} for user {}: {}", 
                                   message.message_id, user_id, e);
                        }
                    }
                }
            }
        });
    }

    /// 启动清理任务
    async fn start_cleanup_task(&self) {
        let queues = self.user_queues.clone();
        let storage = self.storage.clone();
        let running = self.running.clone();
        let enable_persistence = self.config.enable_persistence;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5分钟清理一次
            
            loop {
                interval.tick().await;
                
                {
                    let running = running.lock().await;
                    if !*running {
                        break;
                    }
                }
                
                // 清理过期消息
                let mut total_cleaned = 0;
                {
                    let mut queues = queues.write().await;
                    for (user_id, queue) in queues.iter_mut() {
                        let cleaned = queue.cleanup_expired();
                        if cleaned > 0 {
                            total_cleaned += cleaned;
                            debug!("Cleaned {} expired messages for user {}", cleaned, user_id);
                        }
                    }
                    
                    // 移除空队列
                    queues.retain(|_, queue| !queue.messages.is_empty());
                }
                
                // 清理存储中的过期消息
                if enable_persistence && total_cleaned > 0 {
                    if let Err(e) = storage.cleanup_expired_messages().await {
                        error!("Failed to cleanup expired messages from storage: {}", e);
                    }
                }
                
                if total_cleaned > 0 {
                    info!("Cleaned up {} expired messages", total_cleaned);
                }
            }
        });
    }

    /// 启动投递重试任务
    async fn start_delivery_retry_task(&self) {
        let queues = self.user_queues.clone();
        let running = self.running.clone();
        let interval = self.config.delivery_retry_interval_secs;
        
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval));
            
            loop {
                interval.tick().await;
                
                {
                    let running = running.lock().await;
                    if !*running {
                        break;
                    }
                }
                
                // 重试失败的消息
                let mut queues = queues.write().await;
                for (user_id, queue) in queues.iter_mut() {
                    // 将失败的消息重新设置为待投递状态
                    let mut messages: Vec<_> = queue.messages.drain().collect();
                    for message in &mut messages {
                        if message.status == DeliveryStatus::Failed && message.can_retry() {
                            if message.next_retry_at.map_or(true, |retry_at| chrono::Utc::now() >= retry_at) {
                                message.status = DeliveryStatus::Pending;
                                debug!("Retrying message {} for user {}", message.message_id, user_id);
                            }
                        }
                    }
                    queue.messages = messages.into_iter().collect();
                    queue.rebuild_message_map();
                }
            }
        });
    }
}

/// 创建默认的内存队列管理器
pub async fn create_memory_queue_manager(config: Option<QueueConfig>) -> Result<OfflineQueueManager<MemoryStorage>, ServerError> {
    let storage = MemoryStorage::new();
    let config = config.unwrap_or_default();
    OfflineQueueManager::new(storage, config).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[tokio::test]
    async fn test_queue_manager_basic_operations() {
        let manager = create_memory_queue_manager(None).await.unwrap();
        manager.start().await.unwrap();

        // 添加消息
        let message_id = manager.add_message(
            "user1".to_string(),
            "sender1".to_string(),
            "conv1".to_string(),
            Bytes::from("Hello"),
            "text".to_string(),
            Some(MessagePriority::Normal),
        ).await.unwrap();

        // 获取消息
        let messages = manager.get_messages_for_delivery("user1", Some(10)).await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, message_id);

        // 标记投递成功
        let marked = manager.mark_delivered("user1", &[message_id]).await.unwrap();
        assert_eq!(marked, 1);

        // 获取统计信息
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.user_count, 1);

        manager.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_queue_priority_ordering() {
        let manager = create_memory_queue_manager(None).await.unwrap();
        manager.start().await.unwrap();

        // 添加不同优先级的消息
        let low_id = manager.add_message(
            "user1".to_string(),
            "sender1".to_string(),
            "conv1".to_string(),
            Bytes::from("Low priority"),
            "text".to_string(),
            Some(MessagePriority::Low),
        ).await.unwrap();

        let high_id = manager.add_message(
            "user1".to_string(),
            "sender1".to_string(),
            "conv1".to_string(),
            Bytes::from("High priority"),
            "text".to_string(),
            Some(MessagePriority::High),
        ).await.unwrap();

        // 获取消息，应该按优先级排序
        let messages = manager.get_messages_for_delivery("user1", Some(10)).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].message_id, high_id); // 高优先级应该在前面
        assert_eq!(messages[1].message_id, low_id);

        manager.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_queue_size_limit() {
        let mut config = QueueConfig::default();
        config.max_messages_per_user = 2; // 限制每个用户最多2条消息
        
        let manager = create_memory_queue_manager(Some(config)).await.unwrap();
        manager.start().await.unwrap();

        // 添加3条消息，应该只保留2条
        for i in 1..=3 {
            manager.add_message(
                "user1".to_string(),
                "sender1".to_string(),
                "conv1".to_string(),
                Bytes::from(format!("Message {}", i)),
                "text".to_string(),
                Some(MessagePriority::Normal),
            ).await.unwrap();
        }

        let messages = manager.get_messages_for_delivery("user1", Some(10)).await.unwrap();
        assert_eq!(messages.len(), 2); // 应该只有2条消息

        manager.stop().await.unwrap();
    }
} 