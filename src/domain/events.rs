use serde::{Deserialize, Serialize};

/// Domain Events（领域事件）
///
/// Phase 3: 添加撤销和取消推送事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomainEvent {
    /// 消息已提交（已落库）
    MessageCommitted {
        message_id: u64,
        conversation_id: u64,
        sender_id: u64,
        recipient_id: u64,       // 接收者 ID
        content_preview: String, // 内容预览（用于推送显示）
        message_type: String,
        timestamp: i64,
        device_id: Option<String>, // ✨ Phase 3.5: 可选的设备ID（设备级 Intent）
    },

    /// 消息已撤销（Phase 3）
    MessageRevoked {
        message_id: u64,
        conversation_id: u64,
        revoker_id: u64, // 撤销者 ID
        timestamp: i64,
    },

    /// 用户上线（Phase 3）
    UserOnline {
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },

    /// 消息已通过长连接成功送达（Phase 3.5）
    MessageDelivered {
        message_id: u64,
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },

    /// 设备上线（Phase 3.5，替代 UserOnline 用于设备级取消）
    DeviceOnline {
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },
}
