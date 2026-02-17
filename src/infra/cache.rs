use crate::error::ServerError;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use moka::future::Cache;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;
use std::time::Duration;

#[cfg(feature = "redis")]
use redis::{AsyncCommands, Client as RedisClient};

/// 两层缓存接口 (L1 + L2)
/// L1: Moka 本地高速缓存，低延迟，短TTL
/// L2: Redis 分布式缓存，中TTL，多节点共享
#[async_trait]
pub trait TwoLevelCache<K, V>: Send + Sync
where
    K: Send + Sync + Eq + Hash + Clone + 'static,
    V: Send + Sync + Clone + 'static,
{
    /// 获取缓存值，先查L1，未命中则查L2并回填L1
    async fn get(&self, key: &K) -> Option<V>;

    /// 设置缓存值，同时写入L1和L2
    async fn put(&self, key: K, value: V, ttl_secs: u64);

    /// 删除缓存值，同时从L1和L2删除
    async fn invalidate(&self, key: &K);

    /// 清空L1缓存
    async fn clear(&self);

    /// 批量获取
    async fn mget(&self, keys: &[K]) -> Vec<Option<V>>;

    /// 批量设置
    async fn mput(&self, pairs: Vec<(K, V)>, ttl_secs: u64);
}

/// 支持自动序列化的 Redis 缓存
#[cfg(feature = "redis")]
pub struct SerializedRedisCache<V> {
    client: RedisClient,
    _phantom: std::marker::PhantomData<V>,
}

#[cfg(feature = "redis")]
impl<V> SerializedRedisCache<V>
where
    V: Serialize + DeserializeOwned + Send + Sync + Clone + 'static,
{
    pub fn new(redis_url: &str) -> Result<Self, ServerError> {
        let client = RedisClient::open(redis_url)
            .map_err(|e| ServerError::InternalError(format!("Redis client error: {}", e)))?;

        Ok(Self {
            client,
            _phantom: std::marker::PhantomData,
        })
    }

    async fn get_connection(&self) -> Result<redis::aio::Connection, ServerError> {
        self.client
            .get_async_connection()
            .await
            .map_err(|e| ServerError::InternalError(format!("Redis connection error: {}", e)))
    }

    pub async fn get_bytes(&self, key: &str) -> Option<Vec<u8>> {
        if let Ok(mut conn) = self.get_connection().await {
            conn.get(key).await.ok()
        } else {
            None
        }
    }

    pub async fn set_bytes(&self, key: String, value: Vec<u8>, ttl_secs: u64) {
        if let Ok(mut conn) = self.get_connection().await {
            if ttl_secs > 0 {
                let _: Result<(), _> = conn.set_ex(key, value, ttl_secs as usize).await;
            } else {
                let _: Result<(), _> = conn.set(key, value).await;
            }
        }
    }

    pub async fn del(&self, key: &str) {
        if let Ok(mut conn) = self.get_connection().await {
            let _: Result<(), _> = conn.del(key).await;
        }
    }

    pub async fn mget_bytes(&self, keys: &[String]) -> Vec<Option<Vec<u8>>> {
        if let Ok(mut conn) = self.get_connection().await {
            conn.mget(keys)
                .await
                .unwrap_or_else(|_| vec![None; keys.len()])
        } else {
            vec![None; keys.len()]
        }
    }

    pub async fn mset_bytes(&self, pairs: Vec<(String, Vec<u8>)>, ttl_secs: u64) {
        if let Ok(mut conn) = self.get_connection().await {
            for (key, value) in pairs {
                if ttl_secs > 0 {
                    let _: Result<(), _> = conn.set_ex(key, value, ttl_secs as usize).await;
                } else {
                    let _: Result<(), _> = conn.set(key, value).await;
                }
            }
        }
    }
}

/// L1 + L2 缓存实现 (Moka + Redis)
///
/// 架构图：
/// ┌────────────┐
/// │  Moka (L1) │ ←─ 本地高速缓存，低延迟，短TTL
/// └─────┬──────┘
///       │
///  Miss │
///       ↓
/// ┌────────────┐
/// │ Redis (L2) │ ←─ 共享分布式缓存，中TTL
/// └─────┬──────┘
///       │
///  Miss │
///       ↓
/// ┌────────────┐
/// │ Database   │ ←─ 永久存储，慢
/// └────────────┘
pub struct L1L2Cache<K, V> {
    l1: Cache<K, V>,
    #[cfg(feature = "redis")]
    l2: Option<Arc<SerializedRedisCache<V>>>,
    l1_ttl: Duration,
    l2_ttl: Duration,
}

impl<K, V> L1L2Cache<K, V>
where
    K: Send + Sync + Eq + Hash + Clone + ToString + 'static,
    V: Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    pub fn new(
        max_capacity: u64,
        l1_ttl: Duration,
        l2_ttl: Duration,
        #[cfg(feature = "redis")] redis_cache: Option<Arc<SerializedRedisCache<V>>>,
    ) -> Self {
        let l1 = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(l1_ttl)
            .build();

        Self {
            l1,
            #[cfg(feature = "redis")]
            l2: redis_cache,
            l1_ttl,
            l2_ttl,
        }
    }

    pub fn local_only(max_capacity: u64, l1_ttl: Duration) -> Self {
        let l1 = Cache::builder()
            .max_capacity(max_capacity)
            .time_to_live(l1_ttl)
            .build();

        Self {
            l1,
            #[cfg(feature = "redis")]
            l2: None,
            l1_ttl,
            l2_ttl: Duration::from_secs(0),
        }
    }
}

#[async_trait]
impl<K, V> TwoLevelCache<K, V> for L1L2Cache<K, V>
where
    K: Send + Sync + Eq + Hash + Clone + ToString + 'static,
    V: Send + Sync + Clone + Serialize + DeserializeOwned + 'static,
{
    async fn get(&self, key: &K) -> Option<V> {
        // 先查L1
        if let Some(value) = self.l1.get(key).await {
            return Some(value);
        }

        // L1 未命中，查L2
        #[cfg(feature = "redis")]
        if let Some(l2) = &self.l2 {
            if let Some(value_bytes) = l2.get_bytes(&key.to_string()).await {
                if let Ok(value) = serde_json::from_slice::<V>(&value_bytes) {
                    // 回填到L1
                    self.l1.insert(key.clone(), value.clone()).await;
                    return Some(value);
                }
            }
        }

        None
    }

    async fn put(&self, key: K, value: V, _ttl_secs: u64) {
        // 写入L1
        self.l1.insert(key.clone(), value.clone()).await;

        // 写入L2
        #[cfg(feature = "redis")]
        if let Some(l2) = &self.l2 {
            if let Ok(value_bytes) = serde_json::to_vec(&value) {
                l2.set_bytes(key.to_string(), value_bytes, _ttl_secs as usize)
                    .await;
            }
        }
    }

    async fn invalidate(&self, key: &K) {
        // 删除L1
        self.l1.invalidate(key).await;

        // 删除L2
        #[cfg(feature = "redis")]
        if let Some(l2) = &self.l2 {
            l2.del(&key.to_string()).await;
        }
    }

    async fn clear(&self) {
        // 清空L1
        self.l1.invalidate_all();
        // L2 一般不清空，除非运维需要
    }

    async fn mget(&self, keys: &[K]) -> Vec<Option<V>> {
        let mut results = Vec::with_capacity(keys.len());
        let mut l2_keys = Vec::new();
        let mut l2_indices = Vec::new();

        // 先批量查L1
        for (i, key) in keys.iter().enumerate() {
            if let Some(value) = self.l1.get(key).await {
                results.push(Some(value));
            } else {
                results.push(None);
                l2_keys.push(key.to_string());
                l2_indices.push(i);
            }
        }

        // 批量查L2
        #[cfg(feature = "redis")]
        if let Some(l2) = &self.l2 {
            if !l2_keys.is_empty() {
                let l2_results = l2.mget_bytes(&l2_keys).await;
                for (idx, l2_result) in l2_results.into_iter().enumerate() {
                    if let Some(value_bytes) = l2_result {
                        if let Ok(value) = serde_json::from_slice::<V>(&value_bytes) {
                            let original_idx = l2_indices[idx];
                            results[original_idx] = Some(value.clone());
                            // 回填到L1
                            self.l1.insert(keys[original_idx].clone(), value).await;
                        }
                    }
                }
            }
        }

        results
    }

    async fn mput(&self, pairs: Vec<(K, V)>, _ttl_secs: u64) {
        let mut l2_pairs = Vec::new();

        for (key, value) in pairs {
            // 写入L1
            self.l1.insert(key.clone(), value.clone()).await;

            // 准备L2数据
            if let Ok(value_bytes) = serde_json::to_vec(&value) {
                l2_pairs.push((key.to_string(), value_bytes));
            }
        }

        // 批量写入L2
        #[cfg(feature = "redis")]
        if let Some(l2) = &self.l2 {
            if !l2_pairs.is_empty() {
                l2.mset_bytes(l2_pairs, _ttl_secs as usize).await;
            }
        }
    }
}

/// 用户在线状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserStatus {
    pub user_id: u64,
    pub status: String, // "online", "offline", "away"
    pub devices: Vec<DeviceInfo>,
    pub last_active: DateTime<Utc>,
}

/// 设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub platform: String, // "iOS", "Android", "Web", "Desktop"
    pub session_id: String,
    pub device_name: String,
    pub connected_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

/// 用户设备会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserSessions {
    pub user_id: u64,
    pub device_ids: HashSet<String>,
    pub session_ids: HashSet<String>,
    pub updated_at: DateTime<Utc>,
}

/// 会话列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelList {
    pub user_id: u64,
    pub channels: Vec<ChannelInfo>,
    pub updated_at: DateTime<Utc>,
}

/// 会话信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub channel_id: u64,
    pub channel_type: String, // "direct", "group", "channel"
    pub title: Option<String>,
    pub icon_url: Option<String>,
    pub last_message: Option<String>,
    pub last_message_at: Option<DateTime<Utc>>,
    pub unread_count: i32,
    pub is_pinned: bool,
    pub is_muted: bool,
}

/// 未读消息数
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnreadCounts {
    pub user_id: u64,
    pub counts: HashMap<u64, i32>, // channel_id -> unread_count
    pub updated_at: DateTime<Utc>,
}

/// 群成员列表
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMembers {
    pub group_id: u64,
    pub member_ids: HashSet<u64>,
    pub admin_ids: HashSet<u64>,
    pub owner_id: Option<u64>,
    pub updated_at: DateTime<Utc>,
}

/// 频道订阅者
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSubscribers {
    pub channel_id: u64,
    pub subscriber_ids: HashSet<u64>,
    pub notification_enabled_ids: HashSet<u64>,
    pub updated_at: DateTime<Utc>,
}

/// 好友关系
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRelations {
    pub user_id: u64,
    pub friend_ids: HashSet<u64>,
    pub blocked_ids: HashSet<u64>,
    pub updated_at: DateTime<Utc>,
}

/// 缓存管理器
pub struct CacheManager {
    // 用户在线状态缓存
    pub user_status: Arc<L1L2Cache<u64, UserStatus>>,
    // 用户设备会话缓存
    pub user_sessions: Arc<L1L2Cache<u64, UserSessions>>,
    // 会话列表缓存
    pub channel_list: Arc<L1L2Cache<u64, ChannelList>>,
    // 未读消息数缓存
    pub unread_counts: Arc<L1L2Cache<u64, UnreadCounts>>,
    // 群成员列表缓存
    pub group_members: Arc<L1L2Cache<u64, GroupMembers>>,
    // 频道订阅者缓存
    pub channel_subscribers: Arc<L1L2Cache<u64, ChannelSubscribers>>,
    // 好友关系缓存
    pub friend_relations: Arc<L1L2Cache<u64, FriendRelations>>,
}

impl CacheManager {
    /// 创建仅本地缓存的管理器
    pub fn new() -> Self {
        let l1_ttl = Duration::from_secs(300); // 5分钟
        let capacity = 10000;

        Self {
            user_status: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            user_sessions: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            channel_list: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            unread_counts: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            group_members: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            channel_subscribers: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
            friend_relations: Arc::new(L1L2Cache::local_only(capacity, l1_ttl)),
        }
    }

    /// 创建带 Redis 的混合缓存管理器
    #[cfg(feature = "redis")]
    pub fn with_redis(redis_url: &str) -> Result<Self, ServerError> {
        let l1_ttl = Duration::from_secs(300); // L1: 5分钟
        let l2_ttl = Duration::from_secs(3600); // L2: 1小时
        let capacity = 10000;

        // 创建 Redis 缓存实例
        let user_status_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let user_sessions_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let channel_list_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let unread_counts_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let group_members_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let channel_subscribers_redis = Arc::new(SerializedRedisCache::new(redis_url)?);
        let friend_relations_redis = Arc::new(SerializedRedisCache::new(redis_url)?);

        Ok(Self {
            user_status: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(user_status_redis),
            )),
            user_sessions: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(user_sessions_redis),
            )),
            channel_list: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(channel_list_redis),
            )),
            unread_counts: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(unread_counts_redis),
            )),
            group_members: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(group_members_redis),
            )),
            channel_subscribers: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(channel_subscribers_redis),
            )),
            friend_relations: Arc::new(L1L2Cache::new(
                capacity,
                l1_ttl,
                l2_ttl,
                Some(friend_relations_redis),
            )),
        })
    }

    /// 获取用户在线状态缓存（用于消息路由器）
    pub fn get_user_status_cache(
        &self,
    ) -> Arc<dyn TwoLevelCache<u64, crate::infra::UserOnlineStatus>> {
        // 创建一个适配器来转换类型
        Arc::new(UserStatusCacheAdapter::new(self.user_status.clone()))
    }

    /// 获取离线消息缓存（用于消息路由器）
    pub fn get_offline_message_cache(
        &self,
    ) -> Arc<dyn TwoLevelCache<u64, Vec<crate::infra::OfflineMessage>>> {
        let l1_ttl = Duration::from_secs(3600); // 1小时
        let capacity = 10000;
        Arc::new(L1L2Cache::local_only(capacity, l1_ttl))
    }
}

/// 业务缓存服务接口
#[async_trait]
pub trait BusinessCacheService: Send + Sync {
    /// 获取用户在线状态
    async fn get_user_status(&self, user_id: &u64) -> Result<Option<UserStatus>, ServerError>;

    /// 设置用户在线状态
    async fn set_user_status(&self, status: UserStatus) -> Result<(), ServerError>;

    /// 获取用户会话列表
    async fn get_channel_list(&self, user_id: &u64) -> Result<Option<ChannelList>, ServerError>;

    /// 更新用户会话列表
    async fn update_channel_list(&self, list: ChannelList) -> Result<(), ServerError>;

    /// 获取群成员列表
    async fn get_group_members(&self, group_id: &u64) -> Result<Option<GroupMembers>, ServerError>;

    /// 更新群成员列表
    async fn update_group_members(&self, members: GroupMembers) -> Result<(), ServerError>;

    /// 检查是否为好友
    async fn is_friend(&self, user_id: &u64, friend_id: &u64) -> Result<bool, ServerError>;

    /// 获取未读消息数
    async fn get_unread_count(&self, user_id: &u64, channel_id: &u64) -> Result<i32, ServerError>;

    /// 更新未读消息数
    async fn update_unread_count(
        &self,
        user_id: &u64,
        channel_id: &u64,
        count: i32,
    ) -> Result<(), ServerError>;

    /// 清理用户所有缓存
    async fn clear_user_cache(&self, user_id: &u64) -> Result<(), ServerError>;
}

/// 聊天缓存服务实现
pub struct ChatCacheService {
    cache_manager: Arc<CacheManager>,
}

impl ChatCacheService {
    pub fn new(cache_manager: Arc<CacheManager>) -> Self {
        Self { cache_manager }
    }
}

#[async_trait]
impl BusinessCacheService for ChatCacheService {
    async fn get_user_status(&self, user_id: &u64) -> Result<Option<UserStatus>, ServerError> {
        Ok(self.cache_manager.user_status.get(user_id).await)
    }

    async fn set_user_status(&self, status: UserStatus) -> Result<(), ServerError> {
        self.cache_manager
            .user_status
            .put(status.user_id, status, 3600)
            .await;
        Ok(())
    }

    async fn get_channel_list(&self, user_id: &u64) -> Result<Option<ChannelList>, ServerError> {
        Ok(self.cache_manager.channel_list.get(user_id).await)
    }

    async fn update_channel_list(&self, list: ChannelList) -> Result<(), ServerError> {
        self.cache_manager
            .channel_list
            .put(list.user_id, list, 1800)
            .await;
        Ok(())
    }

    async fn get_group_members(&self, group_id: &u64) -> Result<Option<GroupMembers>, ServerError> {
        Ok(self.cache_manager.group_members.get(group_id).await)
    }

    async fn update_group_members(&self, members: GroupMembers) -> Result<(), ServerError> {
        self.cache_manager
            .group_members
            .put(members.group_id, members, 3600)
            .await;
        Ok(())
    }

    async fn is_friend(&self, user_id: &u64, friend_id: &u64) -> Result<bool, ServerError> {
        if let Some(relations) = self.cache_manager.friend_relations.get(user_id).await {
            Ok(relations.friend_ids.contains(friend_id))
        } else {
            Ok(false)
        }
    }

    async fn get_unread_count(&self, user_id: &u64, channel_id: &u64) -> Result<i32, ServerError> {
        if let Some(counts) = self.cache_manager.unread_counts.get(user_id).await {
            Ok(counts.counts.get(channel_id).copied().unwrap_or(0))
        } else {
            Ok(0)
        }
    }

    async fn update_unread_count(
        &self,
        user_id: &u64,
        channel_id: &u64,
        count: i32,
    ) -> Result<(), ServerError> {
        let mut counts = self
            .cache_manager
            .unread_counts
            .get(user_id)
            .await
            .unwrap_or_else(|| UnreadCounts {
                user_id: *user_id,
                counts: HashMap::new(),
                updated_at: Utc::now(),
            });

        counts.counts.insert(*channel_id, count);
        counts.updated_at = Utc::now();

        self.cache_manager
            .unread_counts
            .put(*user_id, counts, 3600)
            .await;
        Ok(())
    }

    async fn clear_user_cache(&self, user_id: &u64) -> Result<(), ServerError> {
        self.cache_manager.user_status.invalidate(user_id).await;
        self.cache_manager.user_sessions.invalidate(user_id).await;
        self.cache_manager.channel_list.invalidate(user_id).await;
        self.cache_manager.unread_counts.invalidate(user_id).await;
        self.cache_manager
            .friend_relations
            .invalidate(user_id)
            .await;
        Ok(())
    }
}

/// 用户状态缓存适配器
pub struct UserStatusCacheAdapter {
    inner: Arc<L1L2Cache<u64, UserStatus>>,
}

impl UserStatusCacheAdapter {
    pub fn new(inner: Arc<L1L2Cache<u64, UserStatus>>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl TwoLevelCache<u64, crate::infra::UserOnlineStatus> for UserStatusCacheAdapter {
    async fn get(&self, key: &u64) -> Option<crate::infra::UserOnlineStatus> {
        if let Some(user_status) = self.inner.get(key).await {
            // 转换UserStatus到UserOnlineStatus
            let devices = user_status
                .devices
                .into_iter()
                .map(|device| {
                    // 将 String platform 转换为 privchat_protocol::DeviceType
                    let device_type = match device.platform.as_str() {
                        "iOS" | "Mobile" => privchat_protocol::DeviceType::iOS,
                        "Android" => privchat_protocol::DeviceType::Android,
                        "Web" => privchat_protocol::DeviceType::Web,
                        "Desktop" | "MacOS" => privchat_protocol::DeviceType::MacOS,
                        "Windows" => privchat_protocol::DeviceType::Windows,
                        "Linux" | "FreeBSD" | "Unix" => privchat_protocol::DeviceType::Linux,
                        "Tablet" => privchat_protocol::DeviceType::Android, // 平板归类为 Android
                        "IoT" => privchat_protocol::DeviceType::IoT,
                        _ => privchat_protocol::DeviceType::Unknown,
                    };

                    crate::infra::DeviceSession {
                        session_id: device.session_id.clone(),
                        user_id: *key,
                        device_id: device.device_id.clone(),
                        device_type,
                        device_name: device.device_name.clone(),
                        client_version: String::new(),
                        platform: device.platform.clone(),
                        status: if user_status.status == "online" {
                            crate::infra::DeviceStatus::Online
                        } else {
                            crate::infra::DeviceStatus::Offline
                        },
                        connected_at: device.connected_at.timestamp() as u64,
                        last_activity_at: device.last_active.timestamp() as u64,
                        push_token: None,
                        push_enabled: false,
                        device_metadata: None,
                        connection_properties: None,
                    }
                })
                .collect();

            return Some(crate::infra::UserOnlineStatus {
                user_id: *key,
                devices,
                last_activity_at: user_status.last_active.timestamp() as u64,
            });
        }
        None
    }

    async fn put(&self, key: u64, value: crate::infra::UserOnlineStatus, ttl_secs: u64) {
        // 转换UserOnlineStatus到UserStatus
        let devices = value
            .devices
            .into_iter()
            .map(|device| {
                // 将 privchat_protocol::DeviceType 转换为 String
                let platform = format!("{:?}", device.device_type);

                DeviceInfo {
                    device_id: device.device_id,
                    platform,
                    session_id: device.session_id,
                    device_name: device.device_name,
                    connected_at: DateTime::from_timestamp(device.connected_at as i64, 0)
                        .unwrap_or_else(Utc::now),
                    last_active: DateTime::from_timestamp(device.last_activity_at as i64, 0)
                        .unwrap_or_else(Utc::now),
                }
            })
            .collect();

        let user_status = UserStatus {
            user_id: key,
            status: "online".to_string(),
            devices,
            last_active: DateTime::from_timestamp(value.last_activity_at as i64, 0)
                .unwrap_or_else(Utc::now),
        };

        self.inner.put(key, user_status, ttl_secs).await;
    }

    async fn invalidate(&self, key: &u64) {
        self.inner.invalidate(key).await;
    }

    async fn clear(&self) {
        self.inner.clear().await;
    }

    async fn mget(&self, keys: &[u64]) -> Vec<Option<crate::infra::UserOnlineStatus>> {
        // 简单实现，逐个获取
        let mut results = Vec::new();
        for key in keys {
            results.push(self.get(key).await);
        }
        results
    }

    async fn mput(&self, pairs: Vec<(u64, crate::infra::UserOnlineStatus)>, ttl_secs: u64) {
        // 简单实现，逐个设置
        for (key, value) in pairs {
            self.put(key, value, ttl_secs).await;
        }
    }
}
