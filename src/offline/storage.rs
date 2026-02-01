use std::collections::HashMap;
use async_trait::async_trait;
use serde::{Serialize, Deserialize};
use tracing::{debug, info};

use crate::error::ServerError;
use super::message::OfflineMessage;

/// 存储后端 Trait
#[async_trait]
pub trait StorageBackend: Send + Sync {
    /// 存储离线消息
    async fn store_message(&self, message: &OfflineMessage) -> Result<(), ServerError>;
    
    /// 批量存储离线消息
    async fn store_messages(&self, messages: &[OfflineMessage]) -> Result<(), ServerError> {
        for message in messages {
            self.store_message(message).await?;
        }
        Ok(())
    }
    
    /// 获取用户的所有离线消息
    async fn get_messages(&self, user_id: u64) -> Result<Vec<OfflineMessage>, ServerError>;
    
    /// 获取指定数量的用户离线消息
    async fn get_messages_limit(&self, user_id: u64, limit: usize) -> Result<Vec<OfflineMessage>, ServerError>;
    
    /// 删除指定消息
    async fn delete_message(&self, user_id: u64, message_id: u64) -> Result<bool, ServerError>;
    
    /// 批量删除消息
    async fn delete_messages(&self, user_id: u64, message_ids: &[u64]) -> Result<usize, ServerError>;
    
    /// 删除用户的所有离线消息
    async fn delete_all_messages(&self, user_id: u64) -> Result<usize, ServerError>;
    
    /// 清理过期消息
    async fn cleanup_expired_messages(&self) -> Result<usize, ServerError>;
    
    /// 获取存储统计信息
    async fn get_stats(&self) -> Result<StorageStats, ServerError>;
    
    /// 健康检查
    async fn health_check(&self) -> Result<(), ServerError>;
}

/// 存储统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStats {
    /// 总消息数
    pub total_messages: u64,
    /// 待投递消息数
    pub pending_messages: u64,
    /// 已投递消息数
    pub delivered_messages: u64,
    /// 失败消息数
    pub failed_messages: u64,
    /// 过期消息数
    pub expired_messages: u64,
    /// 存储大小（字节）
    pub storage_size_bytes: u64,
    /// 用户数量
    pub user_count: u64,
}

/// 内存存储后端（用于测试和开发）
pub struct MemoryStorage {
    /// 用户消息映射
    messages: tokio::sync::RwLock<HashMap<u64, Vec<OfflineMessage>>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self {
            messages: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl StorageBackend for MemoryStorage {
    async fn store_message(&self, message: &OfflineMessage) -> Result<(), ServerError> {
        let mut messages = self.messages.write().await;
        let user_id = message.user_id.parse::<u64>().unwrap_or(0);
        messages.entry(user_id)
            .or_insert_with(Vec::new)
            .push(message.clone());
        
        debug!("Stored message {} for user {}", message.message_id, message.user_id);
        Ok(())
    }

    async fn get_messages(&self, user_id: u64) -> Result<Vec<OfflineMessage>, ServerError> {
        let messages = self.messages.read().await;
        let mut user_messages = messages.get(&user_id).cloned().unwrap_or_default();
        
        // 按优先级和时间排序
        user_messages.sort();
        
        debug!("Retrieved {} messages for user {}", user_messages.len(), user_id);
        Ok(user_messages)
    }

    async fn get_messages_limit(&self, user_id: u64, limit: usize) -> Result<Vec<OfflineMessage>, ServerError> {
        let mut messages = self.get_messages(user_id).await?;
        messages.truncate(limit);
        Ok(messages)
    }

    async fn delete_message(&self, user_id: u64, message_id: u64) -> Result<bool, ServerError> {
        let mut messages = self.messages.write().await;
        if let Some(user_messages) = messages.get_mut(&user_id) {
            let original_len = user_messages.len();
            user_messages.retain(|msg| msg.message_id != message_id);
            let deleted = user_messages.len() < original_len;
            
            if deleted {
                debug!("Deleted message {} for user {}", message_id, user_id);
            }
            
            Ok(deleted)
        } else {
            Ok(false)
        }
    }

    async fn delete_messages(&self, user_id: u64, message_ids: &[u64]) -> Result<usize, ServerError> {
        let mut deleted_count = 0;
        for &message_id in message_ids {
            if self.delete_message(user_id, message_id).await? {
                deleted_count += 1;
            }
        }
        Ok(deleted_count)
    }

    async fn delete_all_messages(&self, user_id: u64) -> Result<usize, ServerError> {
        let mut messages = self.messages.write().await;
        if let Some(user_messages) = messages.remove(&user_id) {
            let count = user_messages.len();
            debug!("Deleted all {} messages for user {}", count, user_id);
            Ok(count)
        } else {
            Ok(0)
        }
    }

    async fn cleanup_expired_messages(&self) -> Result<usize, ServerError> {
        let mut messages = self.messages.write().await;
        let mut total_removed = 0;

        for (user_id, user_messages) in messages.iter_mut() {
            let original_len = user_messages.len();
            user_messages.retain(|msg| !msg.is_expired());
            let removed = original_len - user_messages.len();
            total_removed += removed;
            
            if removed > 0 {
                debug!("Cleaned up {} expired messages for user {}", removed, user_id);
            }
        }

        // 移除空的用户条目
        messages.retain(|_, msgs| !msgs.is_empty());

        info!("Cleanup completed: removed {} expired messages", total_removed);
        Ok(total_removed)
    }

    async fn get_stats(&self) -> Result<StorageStats, ServerError> {
        let messages = self.messages.read().await;
        let mut stats = StorageStats {
            total_messages: 0,
            pending_messages: 0,
            delivered_messages: 0,
            failed_messages: 0,
            expired_messages: 0,
            storage_size_bytes: 0,
            user_count: messages.len() as u64,
        };

        for user_messages in messages.values() {
            for message in user_messages {
                stats.total_messages += 1;
                stats.storage_size_bytes += message.size_bytes() as u64;

                match message.status {
                    super::message::DeliveryStatus::Pending => stats.pending_messages += 1,
                    super::message::DeliveryStatus::Delivering => stats.pending_messages += 1,
                    super::message::DeliveryStatus::Delivered => stats.delivered_messages += 1,
                    super::message::DeliveryStatus::Failed => stats.failed_messages += 1,
                    super::message::DeliveryStatus::Expired => stats.expired_messages += 1,
                }
            }
        }

        Ok(stats)
    }

    async fn health_check(&self) -> Result<(), ServerError> {
        // 内存存储总是健康的
        Ok(())
    }
}

/// Sled 数据库存储后端
pub struct SledStorage {
    /// Sled 数据库实例
    db: sled::Db,
    /// 数据库路径
    path: String,
}

impl SledStorage {
    /// 创建新的 Sled 存储后端
    pub async fn new(path: &str) -> Result<Self, ServerError> {
        let db = sled::open(path)
            .map_err(|e| ServerError::Internal(format!("Failed to open sled database: {}", e)))?;
        
        info!("Opened Sled database at: {}", path);
        
        Ok(Self {
            db,
            path: path.to_string(),
        })
    }

    /// 获取用户消息的键前缀
    fn user_key_prefix(user_id: u64) -> String {
        format!("user:{}:msg:", user_id)
    }

    /// 获取消息的完整键
    fn message_key(user_id: u64, message_id: u64) -> String {
        format!("user:{}:msg:{}", user_id, message_id)
    }

    /// 序列化消息
    fn serialize_message(message: &OfflineMessage) -> Result<Vec<u8>, ServerError> {
        bincode::serialize(message)
            .map_err(|e| ServerError::Internal(format!("Failed to serialize message: {}", e)))
    }

    /// 反序列化消息
    fn deserialize_message(data: &[u8]) -> Result<OfflineMessage, ServerError> {
        bincode::deserialize(data)
            .map_err(|e| ServerError::Internal(format!("Failed to deserialize message: {}", e)))
    }
}

#[async_trait]
impl StorageBackend for SledStorage {
    async fn store_message(&self, message: &OfflineMessage) -> Result<(), ServerError> {
        let user_id = message.user_id.parse::<u64>().unwrap_or(0);
        let key = Self::message_key(user_id, message.message_id);
        let value = Self::serialize_message(message)?;
        
        self.db.insert(&key, value)
            .map_err(|e| ServerError::Internal(format!("Failed to store message: {}", e)))?;
        
        debug!("Stored message {} for user {} in Sled", message.message_id, message.user_id);
        Ok(())
    }

    async fn get_messages(&self, user_id: u64) -> Result<Vec<OfflineMessage>, ServerError> {
        let prefix = Self::user_key_prefix(user_id);
        let mut messages = Vec::new();

        for result in self.db.scan_prefix(&prefix) {
            let (_key, value) = result
                .map_err(|e| ServerError::Internal(format!("Failed to scan messages: {}", e)))?;
            
            let message = Self::deserialize_message(&value)?;
            messages.push(message);
        }

        // 按优先级和时间排序
        messages.sort();
        
        debug!("Retrieved {} messages for user {} from Sled", messages.len(), user_id);
        Ok(messages)
    }

    async fn get_messages_limit(&self, user_id: u64, limit: usize) -> Result<Vec<OfflineMessage>, ServerError> {
        let mut messages = self.get_messages(user_id).await?;
        messages.truncate(limit);
        Ok(messages)
    }

    async fn delete_message(&self, user_id: u64, message_id: u64) -> Result<bool, ServerError> {
        let key = Self::message_key(user_id, message_id);
        
        let removed = self.db.remove(&key)
            .map_err(|e| ServerError::Internal(format!("Failed to delete message: {}", e)))?
            .is_some();
        
        if removed {
            debug!("Deleted message {} for user {} from Sled", message_id, user_id);
        }
        
        Ok(removed)
    }

    async fn delete_messages(&self, user_id: u64, message_ids: &[u64]) -> Result<usize, ServerError> {
        let mut deleted_count = 0;
        for &message_id in message_ids {
            if self.delete_message(user_id, message_id).await? {
                deleted_count += 1;
            }
        }
        Ok(deleted_count)
    }

    async fn delete_all_messages(&self, user_id: u64) -> Result<usize, ServerError> {
        let prefix = Self::user_key_prefix(user_id);
        let mut deleted_count = 0;

        let keys_to_delete: Result<Vec<_>, _> = self.db.scan_prefix(&prefix)
            .map(|result| result.map(|(key, _)| key))
            .collect();

        let keys_to_delete = keys_to_delete
            .map_err(|e| ServerError::Internal(format!("Failed to scan for deletion: {}", e)))?;

        for key in keys_to_delete {
            if self.db.remove(&key)
                .map_err(|e| ServerError::Internal(format!("Failed to delete key: {}", e)))?
                .is_some() {
                deleted_count += 1;
            }
        }

        debug!("Deleted all {} messages for user {} from Sled", deleted_count, user_id);
        Ok(deleted_count)
    }

    async fn cleanup_expired_messages(&self) -> Result<usize, ServerError> {
        let mut total_removed = 0;
        let mut keys_to_remove = Vec::new();

        // 扫描所有消息，找出过期的
        for result in self.db.iter() {
            let (key, value) = result
                .map_err(|e| ServerError::Internal(format!("Failed to iterate messages: {}", e)))?;
            
            // 只处理消息键
            if let Ok(key_str) = std::str::from_utf8(&key) {
                if key_str.contains(":msg:") {
                    if let Ok(message) = Self::deserialize_message(&value) {
                        if message.is_expired() {
                            keys_to_remove.push(key.to_vec());
                        }
                    }
                }
            }
        }

        // 删除过期消息
        for key in keys_to_remove {
            if self.db.remove(&key)
                .map_err(|e| ServerError::Internal(format!("Failed to remove expired message: {}", e)))?
                .is_some() {
                total_removed += 1;
            }
        }

        info!("Cleanup completed: removed {} expired messages from Sled", total_removed);
        Ok(total_removed)
    }

    async fn get_stats(&self) -> Result<StorageStats, ServerError> {
        let mut stats = StorageStats {
            total_messages: 0,
            pending_messages: 0,
            delivered_messages: 0,
            failed_messages: 0,
            expired_messages: 0,
            storage_size_bytes: 0,
            user_count: 0,
        };

        let mut users = std::collections::HashSet::new();

        for result in self.db.iter() {
            let (key, value) = result
                .map_err(|e| ServerError::Internal(format!("Failed to iterate for stats: {}", e)))?;
            
            if let Ok(key_str) = std::str::from_utf8(&key) {
                if key_str.contains(":msg:") {
                    if let Ok(message) = Self::deserialize_message(&value) {
                        stats.total_messages += 1;
                        stats.storage_size_bytes += value.len() as u64;
                        users.insert(message.user_id.clone());

                        match message.status {
                            super::message::DeliveryStatus::Pending => stats.pending_messages += 1,
                            super::message::DeliveryStatus::Delivering => stats.pending_messages += 1,
                            super::message::DeliveryStatus::Delivered => stats.delivered_messages += 1,
                            super::message::DeliveryStatus::Failed => stats.failed_messages += 1,
                            super::message::DeliveryStatus::Expired => stats.expired_messages += 1,
                        }
                    }
                }
            }
        }

        stats.user_count = users.len() as u64;
        Ok(stats)
    }

    async fn health_check(&self) -> Result<(), ServerError> {
        // 尝试写入和读取一个测试键
        let test_key = "health_check";
        let test_value = b"ok";
        
        self.db.insert(test_key, test_value)
            .map_err(|e| ServerError::Internal(format!("Health check write failed: {}", e)))?;
        
        let retrieved = self.db.get(test_key)
            .map_err(|e| ServerError::Internal(format!("Health check read failed: {}", e)))?;
        
        if retrieved.as_deref() != Some(test_value) {
            return Err(ServerError::Internal("Health check value mismatch".to_string()));
        }
        
        self.db.remove(test_key)
            .map_err(|e| ServerError::Internal(format!("Health check cleanup failed: {}", e)))?;
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use tempfile::TempDir;

    async fn create_test_message(id: u64, user_id: u64) -> OfflineMessage {
        OfflineMessage::new(
            id,
            user_id,
            "sender".to_string(),
            "conv".to_string(),
            Bytes::from("test message"),
            "text".to_string(),
        )
    }

    #[tokio::test]
    async fn test_memory_storage() {
        let storage = MemoryStorage::new();
        let message = create_test_message(1, "user1").await;

        // 测试存储
        storage.store_message(&message).await.unwrap();

        // 测试获取
        let messages = storage.get_messages("user1").await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, 1);

        // 测试删除
        let deleted = storage.delete_message("user1", 1).await.unwrap();
        assert!(deleted);

        let messages = storage.get_messages("user1").await.unwrap();
        assert_eq!(messages.len(), 0);
    }

    #[tokio::test]
    async fn test_sled_storage() {
        let temp_dir = TempDir::new().unwrap();
        let storage = SledStorage::new(temp_dir.path().to_str().unwrap()).await.unwrap();
        let message = create_test_message(1, "user1").await;

        // 测试存储
        storage.store_message(&message).await.unwrap();

        // 测试获取
        let messages = storage.get_messages("user1").await.unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message_id, 1);

        // 测试健康检查
        storage.health_check().await.unwrap();

        // 测试统计
        let stats = storage.get_stats().await.unwrap();
        assert_eq!(stats.total_messages, 1);
        assert_eq!(stats.user_count, 1);
    }
} 