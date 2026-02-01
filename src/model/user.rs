use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use sqlx::Type;

/// 用户信息
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// 用户ID
    pub id: u64,  // 数据库中是 BIGINT
    /// 用户名
    pub username: String,
    /// 密码哈希（bcrypt）
    /// 
    /// - 内置账号系统：必需（由服务器生成）
    /// - 外部账号系统：可选（NULL）
    #[serde(skip_serializing)]  // 永远不要序列化密码哈希
    pub password_hash: Option<String>,
    /// 手机号
    pub phone: Option<String>,
    /// 显示名称
    pub display_name: Option<String>,
    /// 邮箱
    pub email: Option<String>,
    /// 头像URL
    pub avatar_url: Option<String>,
    /// 用户类型：0=普通用户, 1=系统用户, 2=机器人
    pub user_type: i16,
    /// 用户状态
    pub status: UserStatus,
    /// 隐私设置（JSONB）
    pub privacy_settings: Value,
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
    /// 更新时间（数据库存储为 BIGINT 毫秒时间戳）
    pub updated_at: DateTime<Utc>,
    /// 最后活跃时间（数据库存储为 BIGINT 毫秒时间戳）
    pub last_active_at: Option<DateTime<Utc>>,
}

impl User {
    /// 创建新用户
    pub fn new(id: u64, username: String) -> Self {
        let now = Utc::now();
        Self {
            id,
            username,
            password_hash: None,
            phone: None,
            display_name: None,
            email: None,
            avatar_url: None,
            user_type: Self::infer_user_type(id),  // 根据 ID 自动推断类型
            status: UserStatus::Active,
            privacy_settings: Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            last_active_at: Some(now),
        }
    }
    
    /// 创建完整用户信息
    pub fn new_with_details(
        id: u64,
        username: String,
        display_name: Option<String>,
        email: Option<String>,
        avatar_url: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            username,
            password_hash: None,
            phone: None,
            display_name,
            email,
            avatar_url,
            user_type: Self::infer_user_type(id),  // 根据 ID 自动推断类型
            status: UserStatus::Active,
            privacy_settings: Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            last_active_at: Some(now),
        }
    }
    
    /// 创建带密码的用户（用于内置账号系统）
    pub fn new_with_password(
        id: u64,
        username: String,
        password_hash: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            username,
            password_hash: Some(password_hash),
            phone: None,
            display_name: None,
            email: None,
            avatar_url: None,
            user_type: Self::infer_user_type(id),  // 根据 ID 自动推断类型
            status: UserStatus::Active,
            privacy_settings: Value::Object(serde_json::Map::new()),
            created_at: now,
            updated_at: now,
            last_active_at: Some(now),
        }
    }

    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        user_id: i64,  // PostgreSQL BIGINT
        username: String,
        password_hash: Option<String>,
        phone: Option<String>,
        email: Option<String>,
        display_name: Option<String>,
        avatar_url: Option<String>,
        user_type: i16,  // 用户类型
        status: i16,
        privacy_settings: Value,
        created_at: i64,  // 毫秒时间戳
        updated_at: i64,  // 毫秒时间戳
        last_active_at: Option<i64>,  // 毫秒时间戳
    ) -> Self {
        let uid = user_id as u64;
        Self {
            id: uid,
            username,
            password_hash,
            phone,
            display_name,
            email,
            avatar_url,
            // 如果是系统功能用户（ID 1~99），强制设置 user_type = 1
            user_type: if Self::is_system_user(uid) { 1 } else { user_type },
            status: UserStatus::from_i16(status),
            privacy_settings,
            created_at: DateTime::from_timestamp_millis(created_at)
                .unwrap_or_else(|| Utc::now()),
            updated_at: DateTime::from_timestamp_millis(updated_at)
                .unwrap_or_else(|| Utc::now()),
            last_active_at: last_active_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, i16, i16, Value, i64, i64, Option<i64>) {
        (
            self.id as i64,
            self.username.clone(),
            self.password_hash.clone(),
            self.phone.clone(),
            self.email.clone(),
            self.display_name.clone(),
            self.avatar_url.clone(),
            self.user_type,
            self.status.to_i16(),
            self.privacy_settings.clone(),
            self.created_at.timestamp_millis(),
            self.updated_at.timestamp_millis(),
            self.last_active_at.map(|dt| dt.timestamp_millis()),
        )
    }
    
    /// 更新最后活跃时间
    pub fn update_last_active(&mut self) {
        self.last_active_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }
    
    /// 设置用户状态
    pub fn set_status(&mut self, status: UserStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }
    
    /// 判断是否为系统功能用户
    pub fn is_system_user(user_id: u64) -> bool {
        crate::config::is_system_user(user_id)
    }
    
    /// 根据用户 ID 自动推断用户类型
    pub fn infer_user_type(user_id: u64) -> i16 {
        if crate::config::is_system_user(user_id) {
            1  // 系统用户
        } else {
            0  // 普通用户（默认）
        }
    }
    
    /// 获取用户类型名称
    pub fn get_user_type_name(&self) -> &'static str {
        match self.user_type {
            0 => "普通用户",
            1 => "系统用户",
            2 => "机器人",
            _ => "未知",
        }
    }
    
    /// 判断是否为机器人
    pub fn is_bot(&self) -> bool {
        self.user_type == 2
    }
}

/// 用户状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "smallint")]
pub enum UserStatus {
    /// 活跃
    #[default]
    Active = 0,
    /// 非活跃
    Inactive = 1,
    /// 已暂停
    Suspended = 2,
    /// 已删除
    Deleted = 3,
}

impl UserStatus {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => UserStatus::Active,
            1 => UserStatus::Inactive,
            2 => UserStatus::Suspended,
            3 => UserStatus::Deleted,
            _ => UserStatus::Active,
        }
    }

    /// 转换为 i16
    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

/// 设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 设备ID
    pub device_id: String,
    /// 设备类型
    pub device_type: String,
    /// 应用版本
    pub app_version: String,
    /// 连接时间
    pub connected_at: DateTime<Utc>,
    /// 最后活动时间
    pub last_activity: DateTime<Utc>,
}

impl DeviceInfo {
    /// 创建新设备信息
    pub fn new(device_id: String, device_type: String, app_version: String) -> Self {
        let now = Utc::now();
        Self {
            device_id,
            device_type,
            app_version,
            connected_at: now,
            last_activity: now,
        }
    }
    
    /// 更新活动时间
    pub fn update_activity(&mut self) {
        self.last_activity = Utc::now();
    }
}

/// 用户会话（使用 msgtrans::SessionId）
#[derive(Debug, Clone)]
pub struct UserSession {
    /// 用户ID
    pub user_id: u64,
    /// 会话ID
    pub session_id: msgtrans::SessionId,
    /// 设备信息
    pub device_info: DeviceInfo,
    /// 连接时间
    pub connected_at: DateTime<Utc>,
    /// 最后心跳时间
    pub last_heartbeat: DateTime<Utc>,
    /// RPC 上下文
    pub rpc_context: crate::rpc::RpcContext,
}

impl UserSession {
    /// 创建新用户会话
    pub fn new(user_id: u64, session_id: msgtrans::SessionId, device_info: DeviceInfo) -> Self {
        let now = Utc::now();
        // 创建 RPC 上下文
        let rpc_context = crate::rpc::RpcContext::new()
            .with_user_id(user_id.to_string())
            .with_device_id(device_info.device_id.clone());
        
        Self {
            user_id,
            session_id,
            device_info,
            connected_at: now,
            last_heartbeat: now,
            rpc_context,
        }
    }
    
    /// 更新心跳时间
    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = Utc::now();
    }
    
    /// 检查会话是否过期
    pub fn is_expired(&self, timeout_secs: u64) -> bool {
        let now = Utc::now();
        let duration = now.signed_duration_since(self.last_heartbeat);
        duration.num_seconds() > timeout_secs as i64
    }
} 