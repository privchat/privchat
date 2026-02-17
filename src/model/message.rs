use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use privchat_protocol::ContentMessageType;

/// 消息模型
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub message_id: u64,
    pub channel_id: u64,
    pub sender_id: u64,
    /// pts 同步机制（必须保存到数据库）
    pub pts: Option<i64>, // 可选，因为从数据库查询时需要转换
    /// 客户端消息编号（用于去重）
    pub local_message_id: Option<u64>,
    pub content: String,
    pub message_type: ContentMessageType,
    pub metadata: Value,
    pub reply_to_message_id: Option<u64>,
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳，查询时转换为 DateTime）
    pub created_at: DateTime<Utc>,
    /// 更新时间（数据库存储为 BIGINT 毫秒时间戳，查询时转换为 DateTime）
    pub updated_at: DateTime<Utc>,
    pub deleted: bool,
    /// 删除时间（数据库存储为 BIGINT 毫秒时间戳，查询时转换为 DateTime）
    pub deleted_at: Option<DateTime<Utc>>,
    /// 是否已撤回（快速查询）
    pub revoked: bool,
    /// 撤回时间（数据库存储为 BIGINT 毫秒时间戳，查询时转换为 DateTime）
    pub revoked_at: Option<DateTime<Utc>>,
    /// 撤回者ID
    pub revoked_by: Option<u64>,
}

/// 消息状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageStatus {
    pub message_id: u64,
    pub user_id: u64,
    pub device_id: String,
    pub is_read: bool,
    pub is_deleted: bool,
    pub is_pinned: bool,
    pub read_at: Option<DateTime<Utc>>,
    pub deleted_at: Option<DateTime<Utc>>,
}

/// 频道消息模型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub message_id: u64,
    pub channel_id: u64,
    pub publisher_id: Option<u64>,
    pub title: Option<String>,
    pub content: String,
    pub message_type: ContentMessageType,
    pub metadata: Value,
    pub is_urgent: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub published_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// 频道消息状态
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessageStatus {
    pub message_id: u64,
    pub user_id: u64,
    pub device_id: String,
    pub is_read: bool,
    pub is_dismissed: bool,
    pub read_at: Option<DateTime<Utc>>,
    pub dismissed_at: Option<DateTime<Utc>>,
}

/// 消息元数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageMetadata {
    /// @提及的用户ID列表
    pub mentions: Option<Vec<u64>>,
    /// 文件信息
    pub file_info: Option<FileInfo>,
    /// 位置信息
    pub location: Option<LocationInfo>,
    /// 自定义字段
    pub custom: Option<Value>,
}

/// 文件信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub file_name: String,
    pub file_size: i64,
    pub file_type: String,
    pub file_url: String,
    pub thumbnail_url: Option<String>,
}

/// 位置信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationInfo {
    pub latitude: f64,
    pub longitude: f64,
    pub address: Option<String>,
    pub name: Option<String>,
}

impl Message {
    /// 创建新消息
    ///
    /// 注意：message_id 应该由调用者使用 next_message_id() 生成并设置
    pub fn new(
        channel_id: u64,
        sender_id: u64,
        content: String,
        message_type: ContentMessageType,
    ) -> Self {
        // 注意：message_id 应该由调用者使用 next_message_id() 生成
        Self {
            message_id: 0, // 需要调用者设置
            channel_id,
            sender_id,
            pts: None, // 由 PtsGenerator 生成
            local_message_id: None,
            content,
            message_type,
            metadata: Value::Object(serde_json::Map::new()),
            reply_to_message_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        }
    }

    /// 从数据库行创建（处理时间戳转换）
    pub fn from_db_row(
        message_id: i64, // PostgreSQL BIGINT
        channel_id: i64, // PostgreSQL BIGINT
        sender_id: i64,  // PostgreSQL BIGINT
        pts: i64,
        local_message_id: Option<u64>,
        content: String,
        message_type: i16,
        metadata: Value,
        reply_to_message_id: Option<i64>, // PostgreSQL BIGINT
        created_at: i64,                  // 毫秒时间戳
        updated_at: i64,                  // 毫秒时间戳
        deleted: bool,
        deleted_at: Option<i64>, // 毫秒时间戳
        revoked: bool,
        revoked_at: Option<i64>, // 毫秒时间戳
        revoked_by: Option<i64>, // PostgreSQL BIGINT
    ) -> Self {
        Self {
            message_id: message_id as u64,
            channel_id: channel_id as u64,
            sender_id: sender_id as u64,
            pts: Some(pts),
            local_message_id,
            content,
            message_type: ContentMessageType::from_u32(message_type as u32)
                .unwrap_or(ContentMessageType::Text),
            metadata,
            reply_to_message_id: reply_to_message_id.map(|id| id as u64),
            created_at: DateTime::from_timestamp_millis(created_at).unwrap_or_else(|| Utc::now()),
            updated_at: DateTime::from_timestamp_millis(updated_at).unwrap_or_else(|| Utc::now()),
            deleted,
            deleted_at: deleted_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            revoked,
            revoked_at: revoked_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            revoked_by: revoked_by.map(|id| id as u64),
        }
    }

    /// 转换为数据库插入值（返回时间戳）
    pub fn to_db_values(
        &self,
    ) -> (
        i64,
        i64,
        i64,
        i64,
        Option<i64>,
        String,
        i16,
        Value,
        Option<i64>,
        i64,
        i64,
        bool,
        Option<i64>,
        bool,
        Option<i64>,
        Option<i64>,
    ) {
        (
            self.message_id as i64,
            self.channel_id as i64,
            self.sender_id as i64,
            self.pts.unwrap_or(0), // 必须提供 pts
            self.local_message_id.map(|v| v as i64),
            self.content.clone(),
            self.message_type.as_u32() as i16,
            self.metadata.clone(),
            self.reply_to_message_id.map(|id| id as i64),
            self.created_at.timestamp_millis(),
            self.updated_at.timestamp_millis(),
            self.deleted,
            self.deleted_at.map(|dt| dt.timestamp_millis()),
            self.revoked,
            self.revoked_at.map(|dt| dt.timestamp_millis()),
            self.revoked_by.map(|id| id as i64),
        )
    }

    /// 创建回复消息
    ///
    /// 注意：message_id 应该由调用者使用 next_message_id() 生成并设置
    pub fn new_reply(
        channel_id: u64,
        sender_id: u64,
        content: String,
        message_type: ContentMessageType,
        reply_to_message_id: u64,
    ) -> Self {
        // 注意：message_id 应该由调用者使用 next_message_id() 生成
        Self {
            message_id: 0, // 需要调用者设置
            channel_id,
            sender_id,
            pts: None, // 由 PtsGenerator 生成
            local_message_id: None,
            content,
            message_type,
            metadata: Value::Object(serde_json::Map::new()),
            reply_to_message_id: Some(reply_to_message_id),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        }
    }

    /// 标记消息为已删除
    pub fn mark_as_deleted(&mut self) {
        self.deleted = true;
        self.deleted_at = Some(Utc::now());
        self.updated_at = Utc::now();
    }

    /// 检查是否为回复消息
    pub fn is_reply(&self) -> bool {
        self.reply_to_message_id.is_some()
    }

    /// 检查是否为系统消息
    pub fn is_system_message(&self) -> bool {
        matches!(self.message_type, ContentMessageType::System)
    }

    /// 检查是否包含文件
    pub fn has_file(&self) -> bool {
        matches!(
            self.message_type,
            ContentMessageType::Image
                | ContentMessageType::File
                | ContentMessageType::Voice
                | ContentMessageType::Video
        )
    }

    /// 设置元数据
    pub fn set_metadata(&mut self, metadata: MessageMetadata) {
        self.metadata = serde_json::to_value(metadata).unwrap_or_else(|_| Value::Null);
        self.updated_at = Utc::now();
    }

    /// 获取元数据
    pub fn get_metadata(&self) -> Option<MessageMetadata> {
        serde_json::from_value(self.metadata.clone()).ok()
    }

    /// 添加@提及
    pub fn add_mention(&mut self, user_id: u64) {
        let mut metadata = self.get_metadata().unwrap_or_default();
        let mentions = metadata.mentions.get_or_insert_with(Vec::new);
        if !mentions.contains(&user_id) {
            mentions.push(user_id);
            self.set_metadata(metadata);
        }
    }
}

impl MessageStatus {
    /// 创建新的消息状态
    pub fn new(message_id: u64, user_id: u64, device_id: String) -> Self {
        Self {
            message_id,
            user_id,
            device_id,
            is_read: false,
            is_deleted: false,
            is_pinned: false,
            read_at: None,
            deleted_at: None,
        }
    }

    /// 标记为已读
    pub fn mark_as_read(&mut self) {
        self.is_read = true;
        self.read_at = Some(Utc::now());
    }

    /// 标记为已删除
    pub fn mark_as_deleted(&mut self) {
        self.is_deleted = true;
        self.deleted_at = Some(Utc::now());
    }

    /// 切换收藏状态
    pub fn toggle_pin(&mut self) {
        self.is_pinned = !self.is_pinned;
    }
}

impl ChannelMessage {
    /// 创建新的频道消息
    ///
    /// 注意：message_id 应该由调用者使用 next_message_id() 生成并设置
    pub fn new(
        channel_id: u64,
        publisher_id: Option<u64>,
        title: Option<String>,
        content: String,
        message_type: ContentMessageType,
    ) -> Self {
        // 注意：message_id 应该由调用者使用 next_message_id() 生成
        Self {
            message_id: 0, // 需要调用者设置
            channel_id,
            publisher_id,
            title,
            content,
            message_type,
            metadata: Value::Object(serde_json::Map::new()),
            is_urgent: false,
            expires_at: None,
            published_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// 创建紧急消息
    pub fn new_urgent(
        channel_id: u64,
        publisher_id: Option<u64>,
        title: Option<String>,
        content: String,
        message_type: ContentMessageType,
    ) -> Self {
        let mut message = Self::new(channel_id, publisher_id, title, content, message_type);
        message.is_urgent = true;
        message
    }

    /// 设置过期时间
    pub fn set_expires_at(&mut self, expires_at: DateTime<Utc>) {
        self.expires_at = Some(expires_at);
        self.updated_at = Utc::now();
    }

    /// 检查是否已过期
    pub fn is_expired(&self) -> bool {
        self.expires_at.map_or(false, |exp| exp < Utc::now())
    }
}

impl ChannelMessageStatus {
    /// 创建新的频道消息状态
    pub fn new(message_id: u64, user_id: u64, device_id: String) -> Self {
        Self {
            message_id,
            user_id,
            device_id,
            is_read: false,
            is_dismissed: false,
            read_at: None,
            dismissed_at: None,
        }
    }

    /// 标记为已读
    pub fn mark_as_read(&mut self) {
        self.is_read = true;
        self.read_at = Some(Utc::now());
    }

    /// 标记为已忽略
    pub fn mark_as_dismissed(&mut self) {
        self.is_dismissed = true;
        self.dismissed_at = Some(Utc::now());
    }
}

impl Default for MessageMetadata {
    fn default() -> Self {
        Self {
            mentions: None,
            file_info: None,
            location: None,
            custom: None,
        }
    }
}
