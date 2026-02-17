use async_trait::async_trait;
use chrono::{DateTime, Utc};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;
use tracing::info;

use crate::error::ServerError;

/// 简化的缓存存储接口
#[async_trait]
pub trait SimpleCacheStore<K, V>: Send + Sync
where
    K: Send + Sync + Clone + 'static,
    V: Send + Sync + Clone + 'static,
{
    async fn get(&self, key: &K) -> Result<Option<V>, ServerError>;
    async fn set(&self, key: K, value: V, ttl: Duration) -> Result<(), ServerError>;
    async fn delete(&self, key: &K) -> Result<(), ServerError>;
    async fn clear(&self) -> Result<(), ServerError>;
}

/// 简化的L1缓存实现（仅使用Moka）
pub struct SimpleCache<K, V> {
    cache: Cache<K, V>,
}

impl<K, V> SimpleCache<K, V>
where
    K: Send + Sync + Clone + Eq + Hash + 'static,
    V: Send + Sync + Clone + 'static,
{
    pub fn new(max_capacity: u64, ttl: Duration) -> Self {
        let cache = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(ttl)
            .build();

        Self { cache }
    }
}

#[async_trait]
impl<K, V> SimpleCacheStore<K, V> for SimpleCache<K, V>
where
    K: Send + Sync + Clone + Eq + Hash + 'static,
    V: Send + Sync + Clone + 'static,
{
    async fn get(&self, key: &K) -> Result<Option<V>, ServerError> {
        Ok(self.cache.get(key).await)
    }

    async fn set(&self, key: K, value: V, _ttl: Duration) -> Result<(), ServerError> {
        self.cache.insert(key, value).await;
        Ok(())
    }

    async fn delete(&self, key: &K) -> Result<(), ServerError> {
        self.cache.invalidate(key).await;
        Ok(())
    }

    async fn clear(&self) -> Result<(), ServerError> {
        self.cache.invalidate_all();
        Ok(())
    }
}

/// 简化的缓存配置
#[derive(Debug, Clone)]
pub struct SimpleCacheConfig {
    pub max_capacity: u64,
    pub ttl_secs: u64,
}

impl Default for SimpleCacheConfig {
    fn default() -> Self {
        Self {
            max_capacity: 10000,
            ttl_secs: 300, // 5分钟
        }
    }
}

impl SimpleCacheConfig {
    pub fn ttl(&self) -> Duration {
        Duration::from_secs(self.ttl_secs)
    }
}

/// 业务缓存类型定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleUserOnlineStatus {
    pub user_id: String,
    pub is_online: bool,
    pub device_count: u32,
    pub last_active: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleUserSessions {
    pub user_id: String,
    pub session_ids: Vec<String>,
    pub device_ids: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimpleOfflineMessage {
    pub message_id: String,
    pub user_id: String,
    pub device_id: Option<String>,
    pub content: String,
    pub message_type: String,
    pub created_at: DateTime<Utc>,
    pub retry_count: u32,
}

/// 简化的业务缓存管理器
pub struct SimpleBusinessCacheManager {
    pub user_status: Arc<SimpleCache<u64, SimpleUserOnlineStatus>>,
    pub user_sessions: Arc<SimpleCache<u64, SimpleUserSessions>>,
    pub offline_messages: Arc<SimpleCache<u64, Vec<SimpleOfflineMessage>>>,
}

impl SimpleBusinessCacheManager {
    pub fn new(config: &SimpleCacheConfig) -> Self {
        let user_status = Arc::new(SimpleCache::new(config.max_capacity, config.ttl()));
        let user_sessions = Arc::new(SimpleCache::new(config.max_capacity, config.ttl()));
        let offline_messages = Arc::new(SimpleCache::new(config.max_capacity, config.ttl()));

        info!("Simple business cache manager initialized (local-only mode)");

        Self {
            user_status,
            user_sessions,
            offline_messages,
        }
    }

    /// 获取用户在线状态
    pub async fn get_user_status(
        &self,
        user_id: u64,
    ) -> Result<Option<SimpleUserOnlineStatus>, ServerError> {
        self.user_status.get(&user_id).await
    }

    /// 设置用户在线状态
    pub async fn set_user_status(
        &self,
        user_id: u64,
        status: SimpleUserOnlineStatus,
    ) -> Result<(), ServerError> {
        self.user_status
            .set(user_id, status, Duration::from_secs(3600))
            .await
    }

    /// 获取用户会话
    pub async fn get_user_sessions(
        &self,
        user_id: u64,
    ) -> Result<Option<SimpleUserSessions>, ServerError> {
        self.user_sessions.get(&user_id).await
    }

    /// 设置用户会话
    pub async fn set_user_sessions(
        &self,
        user_id: u64,
        sessions: SimpleUserSessions,
    ) -> Result<(), ServerError> {
        self.user_sessions
            .set(user_id, sessions, Duration::from_secs(3600))
            .await
    }

    /// 获取离线消息
    pub async fn get_offline_messages(
        &self,
        user_id: u64,
    ) -> Result<Option<Vec<SimpleOfflineMessage>>, ServerError> {
        self.offline_messages.get(&user_id).await
    }

    /// 设置离线消息
    pub async fn set_offline_messages(
        &self,
        user_id: u64,
        messages: Vec<SimpleOfflineMessage>,
    ) -> Result<(), ServerError> {
        self.offline_messages
            .set(user_id, messages, Duration::from_secs(86400))
            .await // 24小时
    }

    /// 清理用户缓存
    pub async fn clear_user_cache(&self, user_id: u64) -> Result<(), ServerError> {
        let _ = self.user_status.delete(&user_id).await;
        let _ = self.user_sessions.delete(&user_id).await;
        let _ = self.offline_messages.delete(&user_id).await;
        Ok(())
    }
}
