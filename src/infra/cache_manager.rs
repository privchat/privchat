use std::time::Duration;
use moka::future::Cache;
use bb8::Pool;
use bb8_redis::RedisConnectionManager;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::config::{CacheConfig, RedisConfig};
use crate::error::ServerError;

/// 用户会话缓存数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedUserSessions {
    pub user_id: String,
    pub sessions: Vec<String>,
    pub primary_session: Option<String>,
    pub last_updated: chrono::DateTime<chrono::Utc>,
}

/// 离线消息缓存数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedOfflineMessage {
    pub message_id: String,
    pub user_id: String,
    pub sender_id: String,
    pub content: String,
    pub message_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// 聊天消息缓存数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedChatMessage {
    pub message_id: String,
    pub channel_id: String,
    pub sender_id: String,
    pub content: String,
    pub message_type: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub attachments: Vec<String>,
}

/// 用户资料缓存数据
/// 包含业务系统传递的核心字段：账号、昵称、头像、手机号、邮箱
/// 这些数据由业务系统（通行证等）负责管理和同步
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedUserProfile {
    /// 用户ID（IM 系统的 uid）
    pub user_id: String,
    /// 账号（username，类似微信的自定义账号）
    pub username: String,
    /// 昵称（display_name/nickname）
    pub nickname: String,
    /// 头像URL（avatar_url）
    pub avatar_url: Option<String>,
    /// 用户类型（0: 普通用户, 1: 系统用户, 2: 机器人）
    pub user_type: i16,
    /// 手机号（phone，用于搜索）
    pub phone: Option<String>,
    /// 邮箱（email，用于搜索）
    pub email: Option<String>,
}

/// 单条用户设置存储项（ENTITY_SYNC_V1 user_settings，存 Redis）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSettingEntry {
    pub v: u64,
    pub value: serde_json::Value,
}

/// 会话信息缓存数据
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedChannel {
    pub channel_id: String,
    pub channel_type: String,
    pub participants: Vec<String>,
    pub title: Option<String>,
    pub last_message_id: Option<String>,
    pub last_activity: chrono::DateTime<chrono::Utc>,
}

/// Redis 连接池类型
pub type RedisPool = Pool<RedisConnectionManager>;

/// 缓存管理器
/// 支持基于内存大小的 L1 缓存 + Redis L2 缓存
pub struct CacheManager {
    /// L1 缓存 - 用户会话（使用 u64 key，性能更好）
    l1_user_sessions: Cache<u64, CachedUserSessions>,
    /// L1 缓存 - 离线消息
    l1_offline_messages: Cache<u64, Vec<CachedOfflineMessage>>,
    /// L1 缓存 - 聊天消息
    l1_chat_messages: Cache<u64, CachedChatMessage>,
    /// L1 缓存 - 用户资料
    l1_user_profiles: Cache<u64, CachedUserProfile>,
    /// L1 缓存 - 会话信息
    l1_channels: Cache<u64, CachedChannel>,
    /// L1 缓存 - qrcode 索引（qrcode -> user_id）
    l1_qrcode_index: Cache<String, u64>,
    /// L1 缓存 - 隐私设置
    l1_privacy_settings: Cache<u64, crate::model::privacy::UserPrivacySettings>,
    /// L1 缓存 - 搜索记录
    l1_search_records: Cache<u64, crate::model::privacy::SearchRecord>,
    /// L1 缓存 - 名片分享记录
    l1_card_shares: Cache<u64, crate::model::privacy::CardShareRecord>,
    
    /// Redis L2 缓存连接池
    redis_pool: Option<RedisPool>,
    /// Redis 配置
    redis_config: Option<RedisConfig>,
    
    /// 缓存配置
    config: CacheConfig,
}

impl CacheManager {
    // ========== Cache Key 构建方法 ==========
    
    /// 构建用户会话缓存 key
    fn user_sessions_cache_key(user_id: u64) -> String {
        format!("user_sessions:{}", user_id)
    }
    
    /// 构建离线消息缓存 key
    fn offline_messages_cache_key(user_id: u64) -> String {
        format!("offline_messages:{}", user_id)
    }
    
    /// 构建聊天消息缓存 key
    fn chat_message_cache_key(message_id: u64) -> String {
        format!("chat_message:{}", message_id)
    }
    
    /// 构建用户资料缓存 key
    fn user_profile_cache_key(user_id: u64) -> String {
        format!("user_profile:{}", user_id)
    }
    
    /// 构建会话信息缓存 key
    fn channel_cache_key(channel_id: u64) -> String {
        format!("channel:{}", channel_id)
    }
    
    /// 构建隐私设置缓存 key
    fn privacy_settings_cache_key(user_id: u64) -> String {
        format!("privacy_settings:{}", user_id)
    }
    
    /// 构建搜索记录缓存 key
    fn search_record_cache_key(record_id: u64) -> String {
        format!("search_record:{}", record_id)
    }
    
    /// 构建名片分享记录缓存 key
    fn card_share_cache_key(share_id: u64) -> String {
        format!("card_share:{}", share_id)
    }
    
    /// 构建二维码索引缓存 key
    fn qrcode_index_cache_key(qrcode: &str) -> String {
        format!("qrcode:{}", qrcode)
    }

    /// 构建用户设置缓存 key（ENTITY_SYNC_V1 user_settings，scope=None）
    fn user_settings_cache_key(user_id: u64) -> String {
        format!("user_settings:{}", user_id)
    }

    // ========== 构造方法 ==========
    
    /// 创建新的缓存管理器
    pub async fn new(config: CacheConfig) -> Result<Self, ServerError> {
        info!("🔧 Initializing CacheManager with L1 memory: {}MB", config.l1_max_memory_mb);
        
        // 计算各个缓存的容量分配
        let total_memory_bytes = config.l1_max_memory_mb * 1024 * 1024;
        
        // 内存分配策略：
        // - 用户会话: 30% (频繁访问)
        // - 聊天消息: 40% (最大数据量)
        // - 用户资料: 15% (中等访问)
        // - 会话信息: 10% (相对稳定)
        // - 离线消息: 5% (临时数据)
        
        let sessions_capacity = Self::calculate_capacity(total_memory_bytes, 0.30, 200); // 平均200字节
        let messages_capacity = Self::calculate_capacity(total_memory_bytes, 0.40, 500); // 平均500字节
        let profiles_capacity = Self::calculate_capacity(total_memory_bytes, 0.15, 300); // 平均300字节
        let channels_capacity = Self::calculate_capacity(total_memory_bytes, 0.10, 400); // 平均400字节
        let offline_capacity = Self::calculate_capacity(total_memory_bytes, 0.05, 600); // 平均600字节
        
        info!("📊 Cache capacity allocation:");
        info!("  - User sessions: {} items (~{}MB)", sessions_capacity, (sessions_capacity * 200) / (1024 * 1024));
        info!("  - Chat messages: {} items (~{}MB)", messages_capacity, (messages_capacity * 500) / (1024 * 1024));
        info!("  - User profiles: {} items (~{}MB)", profiles_capacity, (profiles_capacity * 300) / (1024 * 1024));
        info!("  - Channels: {} items (~{}MB)", channels_capacity, (channels_capacity * 400) / (1024 * 1024));
        info!("  - Offline messages: {} items (~{}MB)", offline_capacity, (offline_capacity * 600) / (1024 * 1024));
        
        // 创建 L1 缓存
        let l1_user_sessions = Cache::builder()
            .max_capacity(sessions_capacity)
            .time_to_live(config.l1_ttl())
            .build();
            
        let l1_offline_messages = Cache::builder()
            .max_capacity(offline_capacity)
            .time_to_live(config.l1_ttl())
            .build();
            
        let l1_chat_messages = Cache::builder()
            .max_capacity(messages_capacity)
            .time_to_live(config.l1_ttl())
            .build();
            
        let l1_user_profiles = Cache::builder()
            .max_capacity(profiles_capacity)
            .time_to_live(config.l1_ttl())
            .build();
            
        let l1_channels = Cache::builder()
            .max_capacity(channels_capacity)
            .time_to_live(config.l1_ttl())
            .build();
        
        // qrcode 索引缓存（容量与用户资料相同）
        let l1_qrcode_index = Cache::builder()
            .max_capacity(profiles_capacity)
            .time_to_live(config.l1_ttl())
            .build();
        
        // 隐私设置缓存
        let l1_privacy_settings = Cache::builder()
            .max_capacity(profiles_capacity)
            .time_to_live(config.l1_ttl())
            .build();
        
        // 搜索记录缓存（容量较大，因为搜索频繁）
        let l1_search_records = Cache::builder()
            .max_capacity(profiles_capacity * 10) // 搜索记录可能较多
            .time_to_live(Duration::from_secs(3600)) // 1小时过期
            .build();
        
        // 名片分享记录缓存
        let l1_card_shares = Cache::builder()
            .max_capacity(profiles_capacity)
            .time_to_live(Duration::from_secs(7 * 24 * 3600)) // 7天过期
            .build();
        
        // 创建 L2 缓存（如果配置了 Redis）
        let (redis_pool, redis_config) = if let Some(redis_config) = &config.redis {
            info!("🔧 Initializing Redis L2 cache: {}", redis_config.url);
            
            let manager = RedisConnectionManager::new(redis_config.url.clone())
                .map_err(|e| ServerError::Internal(format!("Failed to create Redis manager: {}", e)))?;
            
            let pool = Pool::builder()
                .max_size(redis_config.pool_size)
                .connection_timeout(redis_config.connection_timeout())
                .build(manager)
                .await
                .map_err(|e| ServerError::Internal(format!("Failed to create Redis pool: {}", e)))?;
            
            // 测试连接
            {
                let mut conn = pool.get().await
                    .map_err(|e| ServerError::Internal(format!("Failed to get Redis connection: {}", e)))?;
                
                let _: String = conn.ping().await
                    .map_err(|e| ServerError::Internal(format!("Redis ping failed: {}", e)))?;
            }
            
            info!("✅ Redis L2 cache initialized successfully");
            (Some(pool), Some(redis_config.clone()))
        } else {
            info!("📝 Redis L2 cache not configured, using L1-only mode");
            (None, None)
        };
        
        Ok(Self {
            l1_user_sessions,
            l1_offline_messages,
            l1_chat_messages,
            l1_user_profiles,
            l1_channels,
            l1_qrcode_index,
            l1_privacy_settings,
            l1_search_records,
            l1_card_shares,
            redis_pool,
            redis_config,
            config,
        })
    }
    
    /// 计算缓存容量
    fn calculate_capacity(total_memory_bytes: u64, percentage: f64, avg_item_size: u64) -> u64 {
        let allocated_memory = (total_memory_bytes as f64 * percentage) as u64;
        std::cmp::max(allocated_memory / avg_item_size, 100) // 最少100个条目
    }
    
    /// 获取用户会话
    pub async fn get_user_sessions(&self, user_id: u64) -> Result<Option<CachedUserSessions>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(sessions) = self.l1_user_sessions.get(&user_id).await {
            debug!("L1 cache hit for user sessions: {}", user_id);
            return Ok(Some(sessions));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::user_sessions_cache_key(user_id);
        if let Some(sessions) = self.get_from_redis::<CachedUserSessions>(&redis_key).await? {
            debug!("L2 cache hit for user sessions: {}", user_id);
            // 回填 L1 缓存
            self.l1_user_sessions.insert(user_id, sessions.clone()).await;
            return Ok(Some(sessions));
        }
        
        debug!("Cache miss for user sessions: {}", user_id);
        Ok(None)
    }
    
    /// 设置用户会话
    pub async fn set_user_sessions(&self, user_id: u64, sessions: CachedUserSessions) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_user_sessions.insert(user_id, sessions.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::user_sessions_cache_key(user_id);
        self.set_to_redis(&redis_key, &sessions).await?;
        
        debug!("Updated user sessions cache: {}", user_id);
        Ok(())
    }
    
    /// 获取离线消息
    pub async fn get_offline_messages(&self, user_id: u64) -> Result<Option<Vec<CachedOfflineMessage>>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(messages) = self.l1_offline_messages.get(&user_id).await {
            debug!("L1 cache hit for offline messages: {}", user_id);
            return Ok(Some(messages));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::offline_messages_cache_key(user_id);
        if let Some(messages) = self.get_from_redis::<Vec<CachedOfflineMessage>>(&redis_key).await? {
            debug!("L2 cache hit for offline messages: {}", user_id);
            // 回填 L1 缓存
            self.l1_offline_messages.insert(user_id, messages.clone()).await;
            return Ok(Some(messages));
        }
        
        debug!("Cache miss for offline messages: {}", user_id);
        Ok(None)
    }
    
    /// 设置离线消息
    pub async fn set_offline_messages(&self, user_id: u64, messages: Vec<CachedOfflineMessage>) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_offline_messages.insert(user_id, messages.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::offline_messages_cache_key(user_id);
        self.set_to_redis(&redis_key, &messages).await?;
        
        debug!("Updated offline messages cache: {}", user_id);
        Ok(())
    }
    
    /// 获取聊天消息
    pub async fn get_chat_message(&self, message_id: u64) -> Result<Option<CachedChatMessage>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(message) = self.l1_chat_messages.get(&message_id).await {
            debug!("L1 cache hit for chat message: {}", message_id);
            return Ok(Some(message));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::chat_message_cache_key(message_id);
        if let Some(message) = self.get_from_redis::<CachedChatMessage>(&redis_key).await? {
            debug!("L2 cache hit for chat message: {}", message_id);
            // 回填 L1 缓存
            self.l1_chat_messages.insert(message_id, message.clone()).await;
            return Ok(Some(message));
        }
        
        debug!("Cache miss for chat message: {}", message_id);
        Ok(None)
    }
    
    /// 设置聊天消息
    pub async fn set_chat_message(&self, message_id: u64, message: CachedChatMessage) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_chat_messages.insert(message_id, message.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::chat_message_cache_key(message_id);
        self.set_to_redis(&redis_key, &message).await?;
        
        debug!("Updated chat message cache: {}", message_id);
        Ok(())
    }
    
    /// 获取用户资料
    pub async fn get_user_profile(&self, user_id: u64) -> Result<Option<CachedUserProfile>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(profile) = self.l1_user_profiles.get(&user_id).await {
            debug!("L1 cache hit for user profile: {}", user_id);
            return Ok(Some(profile));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::user_profile_cache_key(user_id);
        if let Some(profile) = self.get_from_redis::<CachedUserProfile>(&redis_key).await? {
            debug!("L2 cache hit for user profile: {}", user_id);
            // 回填 L1 缓存
            self.l1_user_profiles.insert(user_id, profile.clone()).await;
            return Ok(Some(profile));
        }
        
        debug!("Cache miss for user profile: {}", user_id);
        Ok(None)
    }
    
    /// 设置用户资料
    pub async fn set_user_profile(&self, user_id: u64, profile: CachedUserProfile) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_user_profiles.insert(user_id, profile.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::user_profile_cache_key(user_id);
        self.set_to_redis(&redis_key, &profile).await?;
        
        debug!("Updated user profile cache: {}", user_id);
        Ok(())
    }
    
    /// 通过 qrcode 查找用户ID
    pub async fn find_user_by_qrcode(&self, qrcode: &str) -> Result<Option<u64>, ServerError> {
        // 先查 L1 缓存（qrcode 本身就是字符串）
        if let Some(user_id) = self.l1_qrcode_index.get(qrcode).await {
            debug!("L1 cache hit for qrcode: {}", qrcode);
            return Ok(Some(user_id));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::qrcode_index_cache_key(qrcode);
        if let Some(user_id) = self.get_from_redis::<u64>(&redis_key).await? {
            debug!("L2 cache hit for qrcode: {}", qrcode);
            // 回填 L1 缓存
            self.l1_qrcode_index.insert(qrcode.to_string(), user_id).await;
            return Ok(Some(user_id));
        }
        
        debug!("Cache miss for qrcode: {}", qrcode);
        Ok(None)
    }
    
    /// 设置 qrcode 索引
    pub async fn set_qrcode_index(&self, qrcode: &str, user_id: u64) -> Result<(), ServerError> {
        // 更新 L1 缓存（qrcode 本身就是字符串）
        self.l1_qrcode_index.insert(qrcode.to_string(), user_id).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::qrcode_index_cache_key(qrcode);
        self.set_to_redis(&redis_key, &user_id).await?;
        
        debug!("Updated qrcode index: {} -> {}", qrcode, user_id);
        Ok(())
    }
    
    /// 删除 qrcode 索引
    pub async fn remove_qrcode_index(&self, qrcode: &str) -> Result<(), ServerError> {
        // 删除 L1 缓存
        self.l1_qrcode_index.invalidate(qrcode).await;
        
        // 删除 L2 缓存（使用字符串 key）
        if let Some(pool) = &self.redis_pool {
            let mut conn = pool.get().await
                .map_err(|e| ServerError::Internal(format!("Failed to get Redis connection: {}", e)))?;
            let redis_key = Self::qrcode_index_cache_key(qrcode);
            let _: () = conn.del(&redis_key).await
                .map_err(|e| {
                    warn!("Failed to delete qrcode from Redis: {}", e);
                    // 不返回错误，允许 L1 缓存独立工作
                })
                .unwrap_or(());
        }
        
        debug!("Removed qrcode index: {}", qrcode);
        Ok(())
    }
    
    /// 模糊搜索用户（通过 username、phone、email、nickname）
    pub async fn search_users(&self, query: &str) -> Result<Vec<CachedUserProfile>, ServerError> {
        let _query_lower = query.to_lowercase();
        let results = Vec::new();
        
        // 遍历所有用户资料（从 L1 缓存）
        // 注意：这里需要获取所有用户，实际实现可能需要从数据库或 Redis 获取完整列表
        // 当前实现：遍历 L1 缓存中的所有用户
        // TODO: 如果用户量很大，需要优化为从数据库查询或使用全文搜索索引
        
        // 由于 Moka Cache 不提供遍历所有键的方法，我们需要从 Redis 获取所有用户
        // 或者维护一个用户ID列表
        // 这里先实现一个简单版本：如果 Redis 可用，尝试获取所有用户
        
        // 简单实现：只搜索 L1 缓存中的用户（适合小规模用户）
        // 注意：Moka Cache 不提供迭代器，所以这个实现有限制
        // 实际生产环境应该从数据库或维护用户ID列表
        
        // 暂时返回空列表，实际搜索逻辑需要在更高层实现
        // 或者需要维护一个用户ID列表用于搜索
        warn!("search_users: 当前实现有限制，需要维护用户ID列表或从数据库查询");
        Ok(results)
    }
    
    /// 获取会话信息
    pub async fn get_channel(&self, channel_id: u64) -> Result<Option<CachedChannel>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(channel) = self.l1_channels.get(&channel_id).await {
            debug!("L1 cache hit for channel: {}", channel_id);
            return Ok(Some(channel));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::channel_cache_key(channel_id);
        if let Some(channel) = self.get_from_redis::<CachedChannel>(&redis_key).await? {
            debug!("L2 cache hit for channel: {}", channel_id);
            // 回填 L1 缓存
            self.l1_channels.insert(channel_id, channel.clone()).await;
            return Ok(Some(channel));
        }
        
        debug!("Cache miss for channel: {}", channel_id);
        Ok(None)
    }
    
    /// 设置会话信息
    pub async fn set_channel(&self, channel_id: u64, channel: CachedChannel) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_channels.insert(channel_id, channel.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::channel_cache_key(channel_id);
        self.set_to_redis(&redis_key, &channel).await?;
        
        debug!("Updated channel cache: {}", channel_id);
        Ok(())
    }
    
    /// 从 Redis 获取数据
    /// 从 Redis 获取数据
    /// 如果 Redis 不可用，只记录警告，返回 None（不影响 L1 缓存查询）
    async fn get_from_redis<T>(&self, key: &str) -> Result<Option<T>, ServerError>
    where
        T: for<'de> Deserialize<'de>,
    {
        if let Some(pool) = &self.redis_pool {
            // 尝试获取 Redis 连接，失败时只记录警告
            let mut conn = match pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    debug!("Failed to get Redis connection (key: {}): {}, using L1 cache only", key, e);
                    return Ok(None); // Redis 连接失败不影响 L1 缓存查询
                }
            };
            
            // 尝试从 Redis 读取，失败时只记录警告
            let result: Option<String> = match timeout(Duration::from_secs(1), conn.get(key)).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    debug!("Redis get error (key: {}): {}, using L1 cache only", key, e);
                    return Ok(None); // Redis 读取失败不影响 L1 缓存查询
                }
                Err(_) => {
                    debug!("Redis get timeout (key: {}), using L1 cache only", key);
                    return Ok(None); // Redis 超时不影响 L1 缓存查询
                }
            };
            
            if let Some(json_str) = result {
                match serde_json::from_str::<T>(&json_str) {
                    Ok(data) => {
                        debug!("Successfully read from Redis L2 cache: {}", key);
                        return Ok(Some(data));
                    }
                    Err(e) => {
                        warn!("Failed to deserialize from Redis (key: {}): {}, using L1 cache only", key, e);
                        return Ok(None); // 反序列化失败不影响 L1 缓存查询
                    }
                }
            }
        }
        
        Ok(None)
    }
    
    /// 向 Redis 设置数据
    /// 如果 Redis 不可用，只记录警告，不返回错误（L1 缓存已写入成功）
    async fn set_to_redis<T>(&self, key: &str, value: &T) -> Result<(), ServerError>
    where
        T: Serialize,
    {
        if self.config.redis.is_some() {
            let ttl = self.config.l1_ttl_secs; // 使用L1的TTL设置
            let json_str = match serde_json::to_string(value) {
                Ok(s) => s,
                Err(e) => {
                    warn!("Failed to serialize for Redis (key: {}): {}", key, e);
                    return Ok(()); // 序列化失败不影响 L1 缓存
                }
            };
            
            // 尝试获取 Redis 连接
            let pool = match self.redis_pool.as_ref() {
                Some(p) => p,
                None => {
                    warn!("Redis pool not initialized, skipping L2 cache write for key: {}", key);
                    return Ok(()); // Redis 未初始化不影响 L1 缓存
                }
            };
            
            let mut conn = match pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    warn!("Failed to get Redis connection (key: {}): {}, using L1 cache only", key, e);
                    return Ok(()); // Redis 连接失败不影响 L1 缓存
                }
            };
            
            // 尝试写入 Redis，失败时只记录警告
            match timeout(Duration::from_secs(1), conn.set_ex::<&str, String, ()>(key, json_str, ttl)).await {
                Ok(Ok(_)) => {
                    debug!("Successfully wrote to Redis L2 cache: {}", key);
                }
                Ok(Err(e)) => {
                    warn!("Redis set error (key: {}): {}, using L1 cache only", key, e);
                }
                Err(e) => {
                    warn!("Redis set timeout (key: {}): {}, using L1 cache only", key, e);
                }
            }
        }
        
        Ok(())
    }
    
    /// 从 Redis L2 缓存删除指定 key
    /// L1 缓存应该直接调用各自的 remove 方法
    async fn delete_from_redis(&self, key: &str) -> Result<(), ServerError> {
        if let Some(pool) = &self.redis_pool {
            let mut conn = pool.get().await
                .map_err(|e| ServerError::Internal(format!("Failed to get Redis connection: {}", e)))?;
            
            let _: i32 = timeout(Duration::from_secs(1), conn.del(key)).await
                .map_err(|_| ServerError::Internal("Redis delete timeout".to_string()))?
                .map_err(|e| ServerError::Internal(format!("Redis delete error: {}", e)))?;
            
            debug!("Deleted from Redis: {}", key);
        }
        
        Ok(())
    }
    
    /// 从 Redis L2 缓存批量删除多个 keys
    async fn delete_many_from_redis(&self, keys: Vec<String>) -> Result<(), ServerError> {
        if let Some(pool) = &self.redis_pool {
            let count = keys.len();
            let mut conn = pool.get().await
                .map_err(|e| ServerError::Internal(format!("Failed to get Redis connection: {}", e)))?;
            
            for key in keys {
                let _: Result<i32, _> = timeout(
                    Duration::from_secs(1),
                    conn.del(&key)
                ).await
                    .map_err(|_| ServerError::Internal("Redis delete timeout".to_string()))?;
            }
            
            debug!("Deleted {} keys from Redis", count);
        }
        
        Ok(())
    }
    
    // ========== 隐私设置管理 ==========
    
    /// 获取用户隐私设置
    pub async fn get_privacy_settings(&self, user_id: u64) -> Result<Option<crate::model::privacy::UserPrivacySettings>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(settings) = self.l1_privacy_settings.get(&user_id).await {
            debug!("L1 cache hit for privacy settings: {}", user_id);
            return Ok(Some(settings));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::privacy_settings_cache_key(user_id);
        if let Some(settings) = self.get_from_redis::<crate::model::privacy::UserPrivacySettings>(&redis_key).await? {
            debug!("L2 cache hit for privacy settings: {}", user_id);
            self.l1_privacy_settings.insert(user_id, settings.clone()).await;
            return Ok(Some(settings));
        }
        
        debug!("Cache miss for privacy settings: {}", user_id);
        Ok(None)
    }
    
    /// 设置用户隐私设置
    pub async fn set_privacy_settings(&self, user_id: u64, settings: crate::model::privacy::UserPrivacySettings) -> Result<(), ServerError> {
        // 更新 L1 缓存（使用 u64 key）
        self.l1_privacy_settings.insert(user_id, settings.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::privacy_settings_cache_key(user_id);
        self.set_to_redis(&redis_key, &settings).await?;
        
        debug!("Updated privacy settings: {}", user_id);
        Ok(())
    }

    // ========== 用户设置管理（ENTITY_SYNC_V1 user_settings） ==========

    /// 设置单条用户设置，返回新 version
    pub async fn set_user_setting(&self, user_id: u64, setting_key: &str, value: serde_json::Value) -> Result<u64, ServerError> {
        let redis_key = Self::user_settings_cache_key(user_id);
        let mut map: std::collections::HashMap<String, UserSettingEntry> = self
            .get_from_redis(&redis_key)
            .await?
            .unwrap_or_default();
        let next_v = map.values().map(|e| e.v).max().unwrap_or(0).saturating_add(1);
        map.insert(
            setting_key.to_string(),
            UserSettingEntry { v: next_v, value },
        );
        self.set_to_redis(&redis_key, &map).await?;
        Ok(next_v)
    }

    /// 批量设置用户设置，返回新 version（每次写入递增一个 version）
    pub async fn set_user_settings_batch(&self, user_id: u64, settings: &std::collections::HashMap<String, serde_json::Value>) -> Result<u64, ServerError> {
        if settings.is_empty() {
            let redis_key = Self::user_settings_cache_key(user_id);
            let map: std::collections::HashMap<String, UserSettingEntry> = self
                .get_from_redis(&redis_key)
                .await?
                .unwrap_or_default();
            return Ok(map.values().map(|e| e.v).max().unwrap_or(0));
        }
        let redis_key = Self::user_settings_cache_key(user_id);
        let mut map: std::collections::HashMap<String, UserSettingEntry> = self
            .get_from_redis(&redis_key)
            .await?
            .unwrap_or_default();
        let next_v = map.values().map(|e| e.v).max().unwrap_or(0).saturating_add(1);
        for (k, value) in settings {
            map.insert(k.clone(), UserSettingEntry { v: next_v, value: value.clone() });
        }
        self.set_to_redis(&redis_key, &map).await?;
        Ok(next_v)
    }

    /// 获取 user_settings 自 since_version 之后的项，用于 entity/sync_entities
    /// 返回 (items: (setting_key, payload_value, version), next_version, has_more)
    pub async fn get_user_settings_since(
        &self,
        user_id: u64,
        since_version: u64,
        limit: u32,
    ) -> Result<(Vec<(String, serde_json::Value, u64)>, u64, bool), ServerError> {
        let redis_key = Self::user_settings_cache_key(user_id);
        let map: std::collections::HashMap<String, UserSettingEntry> = self
            .get_from_redis(&redis_key)
            .await?
            .unwrap_or_default();
        let mut list: Vec<(String, serde_json::Value, u64)> = map
            .into_iter()
            .filter(|(_, e)| e.v > since_version)
            .map(|(k, e)| (k, e.value, e.v))
            .collect();
        list.sort_by_key(|(_, _, v)| *v);
        let limit = limit as usize;
        let has_more = list.len() > limit;
        if has_more {
            list.truncate(limit);
        }
        let next_version = list.last().map(|(_, _, v)| *v).unwrap_or(since_version);
        Ok((list, next_version, has_more))
    }
    
    // ========== 搜索记录管理 ==========
    
    /// 创建搜索记录
    pub async fn create_search_record(&self, searcher_id: u64, target_id: u64) -> Result<crate::model::privacy::SearchRecord, ServerError> {
        let record = crate::model::privacy::SearchRecord::new(searcher_id, target_id);
        let session_id = record.search_session_id;
        
        // 更新 L1 缓存（使用 u64 key）
        self.l1_search_records.insert(session_id, record.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::search_record_cache_key(session_id);
        self.set_to_redis(&redis_key, &record).await?;
        
        debug!("Created search record: {} -> {}", searcher_id, target_id);
        Ok(record)
    }
    
    /// 获取搜索记录
    pub async fn get_search_record(&self, search_session_id: u64) -> Result<Option<crate::model::privacy::SearchRecord>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(record) = self.l1_search_records.get(&search_session_id).await {
            if !record.is_expired() {
                return Ok(Some(record));
            }
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::search_record_cache_key(search_session_id);
        if let Some(record) = self.get_from_redis::<crate::model::privacy::SearchRecord>(&redis_key).await? {
            if !record.is_expired() {
                self.l1_search_records.insert(search_session_id, record.clone()).await;
                return Ok(Some(record));
            }
        }
        
        Ok(None)
    }
    
    // ========== 名片分享记录管理 ==========
    
    /// 创建名片分享记录
    pub async fn create_card_share(&self, sharer_id: u64, target_user_id: u64, receiver_id: u64) -> Result<crate::model::privacy::CardShareRecord, ServerError> {
        use crate::infra::next_message_id;
        let share_id = next_message_id();
        let record = crate::model::privacy::CardShareRecord::new(
            share_id,
            sharer_id,
            target_user_id,
            receiver_id,
        );
        
        // 更新 L1 缓存（使用 u64 key）
        self.l1_card_shares.insert(share_id, record.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::card_share_cache_key(share_id);
        self.set_to_redis(&redis_key, &record).await?;
        
        debug!("Created card share: {} -> {} (via {})", sharer_id, receiver_id, target_user_id);
        Ok(record)
    }
    
    /// 获取名片分享记录
    pub async fn get_card_share(&self, share_id: u64) -> Result<Option<crate::model::privacy::CardShareRecord>, ServerError> {
        // 先查 L1 缓存（使用 u64 key）
        if let Some(record) = self.l1_card_shares.get(&share_id).await {
            return Ok(Some(record));
        }
        
        // 查 L2 缓存（使用字符串 key）
        let redis_key = Self::card_share_cache_key(share_id);
        if let Some(record) = self.get_from_redis::<crate::model::privacy::CardShareRecord>(&redis_key).await? {
            self.l1_card_shares.insert(share_id, record.clone()).await;
            return Ok(Some(record));
        }
        
        Ok(None)
    }
    
    /// 标记名片分享为已使用
    pub async fn mark_card_share_as_used(&self, share_id: u64, user_id: u64) -> Result<(), ServerError> {
        let mut record = self.get_card_share(share_id).await?
            .ok_or_else(|| ServerError::NotFound(format!("Card share not found: {}", share_id)))?;
        
        record.mark_as_used(user_id);
        
        // 更新 L1 缓存（使用 u64 key）
        self.l1_card_shares.insert(share_id, record.clone()).await;
        
        // 更新 L2 缓存（使用字符串 key）
        let redis_key = Self::card_share_cache_key(share_id);
        self.set_to_redis(&redis_key, &record).await?;
        
        debug!("Marked card share as used: {} by {}", share_id, user_id);
        Ok(())
    }
    
    /// 检查分享者是否已经分享过该用户（用于限制只能分享一次）
    pub async fn has_shared_card(&self, _sharer_id: &str, _target_user_id: u64) -> bool {
        // 由于 Moka Cache 不提供遍历功能，这里需要从 Redis 查询或维护索引
        // 简单实现：检查 Redis 中是否存在 key pattern "card_share:{sharer_id}_{target_user_id}_*"
        // 当前实现：返回 false（表示未分享过），实际应该查询
        // TODO: 实现真正的查询逻辑
        false
    }
    
    /// 获取缓存统计信息
    pub async fn get_stats(&self) -> CacheStats {
        let l1_user_sessions_count = self.l1_user_sessions.entry_count();
        let l1_offline_messages_count = self.l1_offline_messages.entry_count();
        let l1_chat_messages_count = self.l1_chat_messages.entry_count();
        let l1_user_profiles_count = self.l1_user_profiles.entry_count();
        let l1_channels_count = self.l1_channels.entry_count();
        
        let l1_total_entries = l1_user_sessions_count + l1_offline_messages_count + 
                              l1_chat_messages_count + l1_user_profiles_count + l1_channels_count;
        
        // 估算内存使用
        let estimated_memory_mb = (l1_user_sessions_count * 200 + 
                                  l1_offline_messages_count * 600 + 
                                  l1_chat_messages_count * 500 + 
                                  l1_user_profiles_count * 300 + 
                                  l1_channels_count * 400) / (1024 * 1024);
        
        CacheStats {
            l1_total_entries,
            l1_user_sessions_count,
            l1_offline_messages_count,
            l1_chat_messages_count,
            l1_user_profiles_count,
            l1_channels_count,
            l1_estimated_memory_mb: estimated_memory_mb,
            l1_max_memory_mb: self.config.l1_max_memory_mb,
            l2_enabled: self.redis_pool.is_some(),
            l2_pool_size: self.redis_config.as_ref().map(|c| c.pool_size).unwrap_or(0),
            // 添加命中率和请求统计
            l1_hit_rate: 0.0, // 需要实际统计
            l2_hit_rate: 0.0, // 需要实际统计
            total_requests: 0, // 需要实际统计
            l1_hits: 0, // 需要实际统计
            l2_hits: 0, // 需要实际统计
            misses: 0, // 需要实际统计
        }
    }
}

/// 缓存统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub l1_total_entries: u64,
    pub l1_user_sessions_count: u64,
    pub l1_offline_messages_count: u64,
    pub l1_chat_messages_count: u64,
    pub l1_user_profiles_count: u64,
    pub l1_channels_count: u64,
    pub l1_estimated_memory_mb: u64,
    pub l1_max_memory_mb: u64,
    pub l2_enabled: bool,
    pub l2_pool_size: u32,
    // 添加命中率和请求统计
    pub l1_hit_rate: f64,
    pub l2_hit_rate: f64,
    pub total_requests: u64,
    pub l1_hits: u64,
    pub l2_hits: u64,
    pub misses: u64,
} 