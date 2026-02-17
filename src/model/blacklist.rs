//! 黑名单模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 黑名单（对应 privchat_blacklist 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blacklist {
    /// 用户ID
    pub user_id: Uuid,
    /// 被拉黑的用户ID
    pub blocked_user_id: Uuid,
    /// 拉黑时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
}

impl Blacklist {
    /// 创建新的黑名单记录
    pub fn new(user_id: Uuid, blocked_user_id: Uuid) -> Self {
        Self {
            user_id,
            blocked_user_id,
            created_at: Utc::now(),
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        user_id: Uuid,
        blocked_user_id: Uuid,
        created_at: i64, // 毫秒时间戳
    ) -> Self {
        Self {
            user_id,
            blocked_user_id,
            created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, Uuid, i64) {
        (
            self.user_id,
            self.blocked_user_id,
            self.created_at.timestamp_millis(),
        )
    }
}
