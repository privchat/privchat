//! 群组模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use sqlx::Type;

/// 群组状态
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "smallint")]
pub enum GroupStatus {
    /// 活跃
    #[default]
    Active = 0,
    /// 已解散
    Dissolved = 1,
}

impl GroupStatus {
    /// 从 i16 转换
    pub fn from_i16(value: i16) -> Self {
        match value {
            0 => GroupStatus::Active,
            1 => GroupStatus::Dissolved,
            _ => GroupStatus::Active,
        }
    }

    /// 转换为 i16
    pub fn to_i16(self) -> i16 {
        self as i16
    }
}

/// 群组（对应 privchat_groups 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    /// 群组ID
    pub id: u64,  // 数据库中是 BIGINT
    /// 群组名称
    pub name: String,
    /// 群组描述
    pub description: Option<String>,
    /// 群组头像URL
    pub avatar_url: Option<String>,
    /// 群主ID
    pub owner_id: u64,  // 数据库中是 BIGINT
    /// 群设置（JSONB）
    pub settings: Value,
    /// 最大成员数
    pub max_members: i32,
    /// 当前成员数
    pub member_count: i32,
    /// 群组状态
    pub status: GroupStatus,
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
    /// 更新时间（数据库存储为 BIGINT 毫秒时间戳）
    pub updated_at: DateTime<Utc>,
}

impl Group {
    /// 创建新群组
    pub fn new(id: u64, name: String, owner_id: u64) -> Self {
        let now = Utc::now();
        Self {
            id,
            name,
            description: None,
            avatar_url: None,
            owner_id,
            settings: Value::Object(serde_json::Map::new()),
            max_members: 500,
            member_count: 1,  // 包含群主
            status: GroupStatus::Active,
            created_at: now,
            updated_at: now,
        }
    }

    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        group_id: i64,  // PostgreSQL BIGINT
        name: String,
        description: Option<String>,
        avatar_url: Option<String>,
        owner_id: i64,  // PostgreSQL BIGINT
        settings: Value,
        max_members: i32,
        member_count: i32,
        status: i16,
        created_at: i64,  // 毫秒时间戳
        updated_at: i64,  // 毫秒时间戳
    ) -> Self {
        Self {
            id: group_id as u64,
            name,
            description,
            avatar_url,
            owner_id: owner_id as u64,
            settings,
            max_members,
            member_count,
            status: GroupStatus::from_i16(status),
            created_at: DateTime::from_timestamp_millis(created_at)
                .unwrap_or_else(|| Utc::now()),
            updated_at: DateTime::from_timestamp_millis(updated_at)
                .unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (i64, String, Option<String>, Option<String>, i64, Value, i32, i32, i16, i64, i64) {
        (
            self.id as i64,
            self.name.clone(),
            self.description.clone(),
            self.avatar_url.clone(),
            self.owner_id as i64,
            self.settings.clone(),
            self.max_members,
            self.member_count,
            self.status.to_i16(),
            self.created_at.timestamp_millis(),
            self.updated_at.timestamp_millis(),
        )
    }
}

/// 群组成员（对应 privchat_group_members 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupMember {
    /// 群组ID
    pub group_id: u64,
    /// 用户ID
    pub user_id: u64,
    /// 成员角色
    pub role: crate::model::channel::MemberRole,
    /// 群内昵称
    pub nickname: Option<String>,
    /// 个人权限设置（JSONB）
    pub permissions: crate::model::channel::MemberPermissions,
    /// 禁言到期时间（数据库存储为 BIGINT 毫秒时间戳）
    pub mute_until: Option<DateTime<Utc>>,
    /// 加入时间（数据库存储为 BIGINT 毫秒时间戳）
    pub joined_at: DateTime<Utc>,
    /// 离开时间（数据库存储为 BIGINT 毫秒时间戳）
    pub left_at: Option<DateTime<Utc>>,
}

impl GroupMember {
    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        group_id: i64,  // PostgreSQL BIGINT
        user_id: i64,  // PostgreSQL BIGINT
        role: i16,
        nickname: Option<String>,
        permissions: Value,
        mute_until: Option<i64>,  // 毫秒时间戳
        joined_at: i64,  // 毫秒时间戳
        left_at: Option<i64>,  // 毫秒时间戳
    ) -> Self {
        Self {
            group_id: group_id as u64,
            user_id: user_id as u64,
            role: crate::model::channel::MemberRole::from_i16(role),
            nickname,
            permissions: serde_json::from_value(permissions)
                .unwrap_or_else(|_| crate::model::channel::MemberPermissions::default()),
            mute_until: mute_until.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            joined_at: DateTime::from_timestamp_millis(joined_at)
                .unwrap_or_else(|| Utc::now()),
            left_at: left_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (i64, i64, i16, Option<String>, Value, Option<i64>, i64, Option<i64>) {
        (
            self.group_id as i64,
            self.user_id as i64,
            self.role.to_i16(),
            self.nickname.clone(),
            serde_json::to_value(&self.permissions).unwrap_or_else(|_| Value::Object(serde_json::Map::new())),
            self.mute_until.map(|dt| dt.timestamp_millis()),
            self.joined_at.timestamp_millis(),
            self.left_at.map(|dt| dt.timestamp_millis()),
        )
    }
}
