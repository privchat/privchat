use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use sqlx::Type;

/// 好友关系状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "smallint")]
pub enum FriendshipStatus {
    /// 待处理
    #[default]
    Pending = 0,
    /// 已接受
    Accepted = 1,
    /// 已阻止
    Blocked = 2,
    /// 已拒绝（注意：数据库中没有此状态，业务层使用 Blocked 表示）
    Rejected = 3, // 业务层使用，数据库存储为 Blocked (2)
}

impl FriendshipStatus {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => FriendshipStatus::Pending,
            1 => FriendshipStatus::Accepted,
            2 => FriendshipStatus::Blocked, // 数据库中使用 2 表示 Blocked
            _ => FriendshipStatus::Pending,
        }
    }

    /// 转换为 i16（数据库存储值）
    pub fn to_i16(self) -> i16 {
        match self {
            FriendshipStatus::Pending => 0,
            FriendshipStatus::Accepted => 1,
            FriendshipStatus::Rejected => 2, // 数据库中使用 Blocked 值
            FriendshipStatus::Blocked => 2,
        }
    }
}

/// 好友关系
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Friendship {
    /// 关系ID（复合主键，由 user_id 和 friend_id 组成）
    pub id: String, // 业务层使用，数据库中是 (user_id, friend_id) 复合主键
    /// 用户ID（数据库字段）
    pub user1_id: u64, // 数据库中是 BIGINT
    /// 好友ID（数据库字段）
    pub user2_id: u64, // 数据库中是 BIGINT
    /// 关系状态
    pub status: FriendshipStatus,
    /// 来源类型（数据库字段，与添加好友规范一致）
    pub source: Option<String>,
    /// 来源 ID（数据库字段，可追溯）
    pub source_id: Option<String>,
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
    /// 更新时间（数据库存储为 BIGINT 毫秒时间戳）
    pub updated_at: DateTime<Utc>,
}

impl Friendship {
    /// 创建新的好友关系
    pub fn new(user1_id: u64, user2_id: u64) -> Self {
        let now = Utc::now();
        Self {
            id: format!("{}:{}", user1_id, user2_id), // 复合主键的字符串表示
            user1_id,
            user2_id,
            status: FriendshipStatus::Pending,
            source: None,
            source_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        user_id: i64,   // PostgreSQL BIGINT
        friend_id: i64, // PostgreSQL BIGINT
        status: i16,
        source: Option<String>,
        source_id: Option<String>,
        created_at: i64, // 毫秒时间戳
        updated_at: i64, // 毫秒时间戳
    ) -> Self {
        Self {
            id: format!("{}:{}", user_id, friend_id),
            user1_id: user_id as u64,
            user2_id: friend_id as u64,
            status: FriendshipStatus::from_i16(status),
            source,
            source_id,
            created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(|| Utc::now()),
            updated_at: DateTime::from_timestamp_millis(updated_at).unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (i64, i64, i16, Option<String>, Option<String>, i64, i64) {
        (
            self.user1_id as i64,
            self.user2_id as i64,
            self.status.to_i16(),
            self.source.clone(),
            self.source_id.clone(),
            self.created_at.timestamp_millis(),
            self.updated_at.timestamp_millis(),
        )
    }

    /// 更新状态
    pub fn update_status(&mut self, status: FriendshipStatus) {
        self.status = status;
        self.updated_at = Utc::now();
    }
}

/// 好友请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendRequest {
    /// 请求ID
    pub id: u64,
    /// 发送者ID
    pub from_user_id: u64,
    /// 接收者ID
    pub to_user_id: u64,
    /// 请求消息
    pub message: Option<String>,
    /// 状态
    pub status: FriendshipStatus,
    /// 来源（可选）
    pub source: Option<crate::model::privacy::FriendRequestSource>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

impl FriendRequest {
    /// 创建新的好友请求
    pub fn new(from_user_id: u64, to_user_id: u64, message: Option<String>) -> Self {
        let now = Utc::now();
        Self {
            id: 0, // 需要由数据库或调用方生成
            from_user_id,
            to_user_id,
            message,
            status: FriendshipStatus::Pending,
            source: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// 创建带来源的好友请求
    pub fn new_with_source(
        from_user_id: u64,
        to_user_id: u64,
        message: Option<String>,
        source: Option<crate::model::privacy::FriendRequestSource>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: 0, // 需要由数据库或调用方生成
            from_user_id,
            to_user_id,
            message,
            status: FriendshipStatus::Pending,
            source,
            created_at: now,
            updated_at: now,
        }
    }

    /// 接受请求
    pub fn accept(&mut self) {
        self.status = FriendshipStatus::Accepted;
        self.updated_at = Utc::now();
    }

    /// 拒绝请求
    pub fn reject(&mut self) {
        self.status = FriendshipStatus::Rejected;
        self.updated_at = Utc::now();
    }
}
