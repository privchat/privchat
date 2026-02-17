use serde::{Deserialize, Serialize};

/// 推送平台（MVP 只支持这两个）
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PushVendor {
    Apns,
    Fcm,
}

impl PushVendor {
    pub fn as_str(&self) -> &'static str {
        match self {
            PushVendor::Apns => "apns",
            PushVendor::Fcm => "fcm",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "apns" => Some(PushVendor::Apns),
            "fcm" => Some(PushVendor::Fcm),
            _ => None,
        }
    }
}

/// 推送 Payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub r#type: String, // "new_message"
    pub conversation_id: u64,
    pub message_id: u64,
    pub sender_id: u64,
    pub content_preview: String,
}

/// Intent 状态（Phase 3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentStatus {
    Pending,    // 待处理
    Processing, // 处理中
    Sent,       // 已发送
    Cancelled,  // 已取消（设备上线）
    Revoked,    // 已撤销（消息撤销）
}

/// PushIntent（设备级，Phase 3.5）
#[derive(Debug, Clone)]
pub struct PushIntent {
    pub intent_id: String,
    pub message_id: u64,
    pub conversation_id: u64,
    pub user_id: u64,
    pub device_id: String, // ✨ Phase 3.5: 设备级 Intent
    pub sender_id: u64,
    pub payload: PushPayload,
    pub created_at: i64,
    pub status: IntentStatus,
}

impl PushIntent {
    pub fn new(
        intent_id: String,
        message_id: u64,
        conversation_id: u64,
        user_id: u64,
        device_id: String, // ✨ Phase 3.5: 新增参数
        sender_id: u64,
        payload: PushPayload,
        created_at: i64,
    ) -> Self {
        Self {
            intent_id,
            message_id,
            conversation_id,
            user_id,
            device_id, // ✨ Phase 3.5: 新增字段
            sender_id,
            payload,
            created_at,
            status: IntentStatus::Pending,
        }
    }
}

/// PushTask（设备级）
#[derive(Debug, Clone)]
pub struct PushTask {
    pub task_id: String,
    pub intent_id: String,
    pub user_id: u64,
    pub device_id: String,
    pub vendor: PushVendor,
    pub push_token: String,
    pub payload: PushPayload,
}
