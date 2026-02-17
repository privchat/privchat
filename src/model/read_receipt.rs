//! 已读回执模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 已读回执（对应 privchat_read_receipts 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipt {
    /// 消息ID
    pub message_id: Uuid,
    /// 用户ID
    pub user_id: Uuid,
    /// 会话ID
    pub channel_id: Uuid,
    /// 已读时间（数据库存储为 BIGINT 毫秒时间戳）
    pub read_at: DateTime<Utc>,
}

impl ReadReceipt {
    /// 创建新的已读回执
    pub fn new(message_id: Uuid, user_id: Uuid, channel_id: Uuid) -> Self {
        Self {
            message_id,
            user_id,
            channel_id,
            read_at: Utc::now(),
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        message_id: Uuid,
        user_id: Uuid,
        channel_id: Uuid,
        read_at: i64, // 毫秒时间戳
    ) -> Self {
        Self {
            message_id,
            user_id,
            channel_id,
            read_at: DateTime::from_timestamp_millis(read_at).unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, Uuid, Uuid, i64) {
        (
            self.message_id,
            self.user_id,
            self.channel_id,
            self.read_at.timestamp_millis(),
        )
    }
}
