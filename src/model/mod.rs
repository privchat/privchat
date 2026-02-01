//! 数据模型模块

#![allow(ambiguous_glob_reexports)]

// 基础模型
pub mod device;
pub mod friend;
pub mod message;
pub mod notification;
pub mod user;

// 频道模型（合并了原 channel 和 channel）
pub mod channel;

// 隐私和权限模型
pub mod privacy;

// pts 同步相关模型
pub mod pts;

// QR 码相关模型
pub mod qrcode;

// 数据库相关模型
pub mod read_receipt;
pub mod user_channel;
pub mod group;
pub mod blacklist;
pub mod device_sync_state;
pub mod file_upload;

// 重新导出常用类型
pub use channel::*;
pub use device::*;
pub use friend::*;
pub use message::*;
pub use notification::*;
pub use user::*;
pub use privacy::*;
pub use pts::*;
pub use qrcode::*;
pub use read_receipt::*;
pub use user_channel::*;
pub use group::*;
pub use blacklist::*;
pub use file_upload::*;

use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

/// 领域事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomainEvent {
    /// 用户创建
    UserCreated {
        user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 用户更新
    UserUpdated {
        user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 用户删除
    UserDeleted {
        user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 消息发送
    MessageSent {
        message_id: String,
        channel_id: String,
        sender_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 消息接收
    MessageReceived {
        message_id: String,
        channel_id: String,
        receiver_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 频道创建
    ChannelCreated {
        channel_id: String,
        creator_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 频道更新
    ChannelUpdated {
        channel_id: String,
        updater_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 好友请求
    FriendRequestSent {
        from_user_id: String,
        to_user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 好友请求接受
    FriendRequestAccepted {
        from_user_id: String,
        to_user_id: String,
        timestamp: DateTime<Utc>,
    },
    /// 好友删除
    FriendRemoved {
        user_id: String,
        friend_id: String,
        timestamp: DateTime<Utc>,
    },
}

/// 设备类型
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(non_camel_case_types)]
pub enum DeviceType {
    /// iOS设备 (iPhone/iPad)
    iOS,
    /// Android设备 (手机/平板)
    Android,
    /// Web浏览器
    Web,
    /// macOS桌面应用
    MacOS,
    /// Windows桌面应用
    Windows,
    /// Linux/Unix桌面应用 (包括 FreeBSD 等)
    Linux,
    /// IoT设备 (智能音箱、智能硬件、手表等)
    IoT,
    /// 未知设备
    Unknown,
    /// 其他设备
    Other(String),
}

impl DeviceType {
    /// 转换为字符串
    pub fn as_str(&self) -> &str {
        match self {
            DeviceType::iOS => "ios",
            DeviceType::Android => "android",
            DeviceType::Web => "web",
            DeviceType::MacOS => "macos",
            DeviceType::Windows => "windows",
            DeviceType::Linux => "linux",
            DeviceType::IoT => "iot",
            DeviceType::Unknown => "unknown",
            DeviceType::Other(s) => s,
        }
    }
    
    /// 从字符串创建
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ios" => DeviceType::iOS,
            "android" => DeviceType::Android,
            "web" => DeviceType::Web,
            "macos" | "desktop" => DeviceType::MacOS,
            "windows" => DeviceType::Windows,
            "linux" | "freebsd" | "unix" => DeviceType::Linux,
            "iot" => DeviceType::IoT,
            "unknown" => DeviceType::Unknown,
            _ => DeviceType::Other(s.to_string()),
        }
    }
}

/// 消息类型
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MessageType {
    /// 文本消息
    Text,
    /// 图片消息
    Image,
    /// 语音消息
    Audio,
    /// 视频消息
    Video,
    /// 文件消息
    File,
    /// 位置消息
    Location,
    /// 系统消息（包括系统通知、群组操作等）
    System,
    /// 名片消息
    ContactCard,
    /// 表情包消息
    Sticker,
    /// 批量转发消息
    Forward,
    /// 自定义消息
    Custom(String),
}

impl MessageType {
    /// 转换为字符串
    pub fn as_str(&self) -> &str {
        match self {
            MessageType::Text => "text",
            MessageType::Image => "image",
            MessageType::Audio => "audio",
            MessageType::Video => "video",
            MessageType::File => "file",
            MessageType::Location => "location",
            MessageType::System => "system",
            MessageType::ContactCard => "contact_card",
            MessageType::Sticker => "sticker",
            MessageType::Forward => "forward",
            MessageType::Custom(s) => s,
        }
    }
    
    /// 从字符串创建
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "text" => MessageType::Text,
            "image" => MessageType::Image,
            "audio" => MessageType::Audio,
            "video" => MessageType::Video,
            "file" => MessageType::File,
            "location" => MessageType::Location,
            "system" => MessageType::System,
            "contact_card" => MessageType::ContactCard,
            "sticker" => MessageType::Sticker,
            "forward" => MessageType::Forward,
            _ => MessageType::Custom(s.to_string()),
        }
    }
}

/// 消息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// 消息ID
    pub id: u64,
    /// 频道ID
    pub channel_id: u64,
    /// 发送者ID
    pub sender_id: u64,
    /// 消息类型
    pub message_type: MessageType,
    /// 消息内容
    pub content: String,
    /// 消息元数据
    pub metadata: Option<serde_json::Value>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

impl Message {
    /// 创建新消息
    pub fn new(
        id: u64,
        channel_id: u64,
        sender_id: u64,
        message_type: MessageType,
        content: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            channel_id,
            sender_id,
            message_type,
            content,
            metadata: None,
            created_at: now,
            updated_at: now,
        }
    }
    
    /// 设置元数据
    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
} 