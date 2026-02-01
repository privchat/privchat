// Infrastructure layer - 基础设施层
// 负责各种基础服务：缓存、消息路由、离线投递、会话管理等

use serde::{Serialize, Deserialize};

// TODO: cache 模块依赖 message_router，需要一起集成
// pub mod cache;
pub mod cache_manager;
pub mod cache_manager_simple;
pub mod online_status_manager;
pub mod presence_manager;
pub mod presence_manager_with_db;
pub mod database;
pub mod logger;
pub mod message_router;
pub mod metrics;
pub mod offline_worker;
pub mod cache;
pub mod redis;
pub mod session_manager;
pub mod auth_whitelist;
pub mod snowflake;
pub mod connection_manager;  // ✨ 新增
pub mod event_bus;  // ✨ 新增：事件总线
// pub mod redis_cache;
// pub mod service_protocol;
// pub mod service_registry;

// 重新导出主要类型
pub use cache_manager::{
    CacheManager, CachedUserSessions, CachedOfflineMessage, CachedChatMessage,
    CachedUserProfile, CachedChannel, CacheStats, RedisPool,
};
pub use cache_manager_simple::{
    SimpleCacheStore, SimpleCache, SimpleBusinessCacheManager, SimpleCacheConfig,
    SimpleUserOnlineStatus, SimpleUserSessions, SimpleOfflineMessage,
};
pub use online_status_manager::{
    OnlineStatusManager, OnlineSession, OnlineStatusStats,
};
pub use presence_manager::{
    PresenceManager as BasicPresenceManager,
    PresenceConfig as BasicPresenceConfig,
    PresenceStats as BasicPresenceStats,
};
pub use presence_manager_with_db::{
    PresenceManagerWithDb as PresenceManager,
    PresenceConfig,
    PresenceStats,
};
pub use message_router::{
    MessageRouter, MessageRouterConfig, RouteResult, SessionManagerAdapter,
};
pub use offline_worker::{
    OfflineMessageWorker, OfflineWorkerConfig, DeliveryStats,
};
pub use cache::{
    TwoLevelCache, L1L2Cache, CacheManager as NewCacheManager,
    UserStatusCacheAdapter,
};
pub use session_manager::{SessionManager, SessionInfo};
pub use auth_whitelist::{
    is_anonymous_message_type, is_anonymous_rpc_route,
    list_anonymous_message_types, list_anonymous_rpc_routes,
};
pub use connection_manager::{ConnectionManager, DeviceConnection};  // ✨ 新增
pub use event_bus::EventBus;  // ✨ 新增

// 数据库连接管理
pub use database::Database;

// Snowflake ID生成器
pub use snowflake::next_message_id;
// pub use logger::setup_logger;
// pub use metrics::Metrics;
// pub use redis::RedisManager;

// 基础类型定义
pub type SessionId = String;
pub type UserId = u64;
pub type DeviceId = String;  // 保留UUID字符串

/// 设备状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeviceStatus {
    Online,
    Offline,
    Away,
    Busy,
}

/// 用户在线状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserOnlineStatus {
    pub user_id: u64,
    pub devices: Vec<DeviceSession>,
    pub last_activity_at: u64,
}

impl Default for UserOnlineStatus {
    fn default() -> Self {
        Self {
            user_id: 0,
            devices: Vec::new(),
            last_activity_at: 0,
        }
    }
}

/// 设备会话信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceSession {
    pub session_id: String,
    pub user_id: u64,
    pub device_id: String,
    pub device_type: privchat_protocol::DeviceType,
    pub device_name: String,
    pub client_version: String,
    pub platform: String,
    pub status: DeviceStatus,
    pub connected_at: u64,
    pub last_activity_at: u64,
    pub push_token: Option<String>,
    pub push_enabled: bool,
    pub device_metadata: Option<serde_json::Value>,
    pub connection_properties: Option<serde_json::Value>,
}

/// 离线消息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineMessage {
    pub message_id: u64,
    pub user_id: u64,
    pub channel_id: u64,
    pub sender_id: u64,
    pub content: String,
    pub message_type: String,
    pub priority: MessagePriority,
    pub created_at: u64,
    pub retry_count: u32,
    pub max_retry_count: u32,
    pub next_retry_at: u64,
    pub expires_at: u64,
}

/// 消息优先级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessagePriority {
    Low = 0,
    Normal = 1,
    High = 2,
    Urgent = 3,
}

impl Default for MessagePriority {
    fn default() -> Self {
        Self::Normal
    }
}

/// 用户状态（简化版本）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserStatus {
    Active,
    Inactive,
    Suspended,
    Deleted,
} 