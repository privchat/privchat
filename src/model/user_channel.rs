//! 用户会话列表模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 用户会话列表（对应 privchat_user_channels 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserChannel {
    /// 用户ID
    pub user_id: Uuid,
    /// 会话ID
    pub channel_id: Uuid,
    /// 最后已读消息ID
    pub last_read_message_id: Option<Uuid>,
    /// 最后已读时间（数据库存储为 BIGINT 毫秒时间戳）
    pub last_read_at: Option<DateTime<Utc>>,
    /// 未读消息数
    pub unread_count: i32,
    /// 是否置顶
    pub is_pinned: bool,
    /// 是否静音
    pub is_muted: bool,
    /// 更新时间（数据库存储为 BIGINT 毫秒时间戳）
    pub updated_at: DateTime<Utc>,
}

impl UserChannel {
    /// 创建新的用户会话列表项
    pub fn new(user_id: Uuid, channel_id: Uuid) -> Self {
        Self {
            user_id,
            channel_id,
            last_read_message_id: None,
            last_read_at: None,
            unread_count: 0,
            is_pinned: false,
            is_muted: false,
            updated_at: Utc::now(),
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        user_id: Uuid,
        channel_id: Uuid,
        last_read_message_id: Option<Uuid>,
        last_read_at: Option<i64>,  // 毫秒时间戳
        unread_count: i32,
        is_pinned: bool,
        is_muted: bool,
        updated_at: i64,  // 毫秒时间戳
    ) -> Self {
        Self {
            user_id,
            channel_id,
            last_read_message_id,
            last_read_at: last_read_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            unread_count,
            is_pinned,
            is_muted,
            updated_at: DateTime::from_timestamp_millis(updated_at)
                .unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, Uuid, Option<Uuid>, Option<i64>, i32, bool, bool, i64) {
        (
            self.user_id,
            self.channel_id,
            self.last_read_message_id,
            self.last_read_at.map(|dt| dt.timestamp_millis()),
            self.unread_count,
            self.is_pinned,
            self.is_muted,
            self.updated_at.timestamp_millis(),
        )
    }

    /// 标记为已读
    pub fn mark_as_read(&mut self, message_id: Uuid) {
        self.last_read_message_id = Some(message_id);
        self.last_read_at = Some(Utc::now());
        self.unread_count = 0;
        self.updated_at = Utc::now();
    }

    /// 增加未读数
    pub fn increment_unread(&mut self) {
        self.unread_count += 1;
        self.updated_at = Utc::now();
    }

    /// 清空未读数
    pub fn clear_unread(&mut self) {
        self.unread_count = 0;
        self.updated_at = Utc::now();
    }
}
