use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// 消息优先级
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MessagePriority {
    /// 低优先级 - 普通聊天消息
    Low = 0,
    /// 正常优先级 - 大部分消息
    Normal = 1,
    /// 高优先级 - 重要通知
    High = 2,
    /// 紧急优先级 - 系统消息、好友请求等
    Urgent = 3,
}

impl Default for MessagePriority {
    fn default() -> Self {
        MessagePriority::Normal
    }
}

/// 消息投递状态
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeliveryStatus {
    /// 等待投递
    Pending,
    /// 投递中
    Delivering,
    /// 投递成功
    Delivered,
    /// 投递失败
    Failed,
    /// 已过期
    Expired,
}

/// 离线消息结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineMessage {
    /// 消息唯一ID
    pub message_id: u64,
    /// 目标用户ID
    pub user_id: String,
    /// 发送者ID
    pub sender_id: String,
    /// 会话ID
    pub channel_id: String,
    /// 消息内容（序列化后的字节）
    pub payload: Bytes,
    /// 消息类型
    pub message_type: String,
    /// 消息优先级
    pub priority: MessagePriority,
    /// 投递状态
    pub status: DeliveryStatus,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 过期时间（可选）
    pub expire_at: Option<DateTime<Utc>>,
    /// 重试次数
    pub retry_count: u8,
    /// 最大重试次数
    pub max_retries: u8,
    /// 下次重试时间
    pub next_retry_at: Option<DateTime<Utc>>,
    /// 消息元数据
    pub metadata: std::collections::HashMap<String, String>,
}

impl OfflineMessage {
    /// 创建新的离线消息
    pub fn new(
        message_id: u64,
        user_id: String,
        sender_id: String,
        channel_id: String,
        payload: Bytes,
        message_type: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            message_id,
            user_id,
            sender_id,
            channel_id,
            payload,
            message_type,
            priority: MessagePriority::default(),
            status: DeliveryStatus::Pending,
            created_at: now,
            expire_at: Some(now + chrono::Duration::days(7)), // 默认7天过期
            retry_count: 0,
            max_retries: 3,
            next_retry_at: None,
            metadata: std::collections::HashMap::new(),
        }
    }

    /// 设置消息优先级
    pub fn with_priority(mut self, priority: MessagePriority) -> Self {
        self.priority = priority;
        self
    }

    /// 设置过期时间
    pub fn with_expiry(mut self, duration: chrono::Duration) -> Self {
        self.expire_at = Some(self.created_at + duration);
        self
    }

    /// 设置最大重试次数
    pub fn with_max_retries(mut self, max_retries: u8) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// 添加元数据
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }

    /// 检查消息是否已过期
    pub fn is_expired(&self) -> bool {
        if let Some(expire_at) = self.expire_at {
            Utc::now() > expire_at
        } else {
            false
        }
    }

    /// 检查是否可以重试
    pub fn can_retry(&self) -> bool {
        self.retry_count < self.max_retries && !self.is_expired()
    }

    /// 标记投递失败并准备重试
    pub fn mark_failed_for_retry(&mut self) -> bool {
        if self.can_retry() {
            self.retry_count += 1;
            self.status = DeliveryStatus::Pending;

            // 计算下次重试时间（指数退避）
            let delay_seconds = 2_u64.pow(self.retry_count as u32).min(300); // 最大5分钟
            self.next_retry_at = Some(Utc::now() + chrono::Duration::seconds(delay_seconds as i64));

            true
        } else {
            self.status = DeliveryStatus::Failed;
            false
        }
    }

    /// 标记投递成功
    pub fn mark_delivered(&mut self) {
        self.status = DeliveryStatus::Delivered;
        self.next_retry_at = None;
    }

    /// 标记为投递中
    pub fn mark_delivering(&mut self) {
        self.status = DeliveryStatus::Delivering;
    }

    /// 标记为过期
    pub fn mark_expired(&mut self) {
        self.status = DeliveryStatus::Expired;
    }

    /// 检查是否准备好投递
    pub fn is_ready_for_delivery(&self) -> bool {
        match self.status {
            DeliveryStatus::Pending => {
                if let Some(next_retry_at) = self.next_retry_at {
                    Utc::now() >= next_retry_at
                } else {
                    true
                }
            }
            _ => false,
        }
    }

    /// 获取消息大小（字节）
    pub fn size_bytes(&self) -> usize {
        self.payload.len()
            + self.user_id.len()
            + self.sender_id.len()
            + self.channel_id.len()
            + self.message_type.len()
            + self
                .metadata
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>()
            + 256 // 其他字段的估算大小
    }

    /// 获取消息年龄
    pub fn age(&self) -> chrono::Duration {
        Utc::now() - self.created_at
    }

    /// 获取剩余生存时间
    pub fn ttl(&self) -> Option<chrono::Duration> {
        self.expire_at.map(|expire_at| expire_at - Utc::now())
    }
}

impl PartialEq for OfflineMessage {
    fn eq(&self, other: &Self) -> bool {
        self.message_id == other.message_id
    }
}

impl Eq for OfflineMessage {}

impl PartialOrd for OfflineMessage {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OfflineMessage {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // 首先按优先级排序（高优先级在前）
        match other.priority.cmp(&self.priority) {
            std::cmp::Ordering::Equal => {
                // 优先级相同时，按创建时间排序（早创建的在前）
                self.created_at.cmp(&other.created_at)
            }
            other_order => other_order,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_creation() {
        let message = OfflineMessage::new(
            1,
            "user1".to_string(),
            "user2".to_string(),
            "conv1".to_string(),
            Bytes::from("hello"),
            "text".to_string(),
        );

        assert_eq!(message.message_id, 1);
        assert_eq!(message.user_id, "user1");
        assert_eq!(message.priority, MessagePriority::Normal);
        assert_eq!(message.status, DeliveryStatus::Pending);
        assert_eq!(message.retry_count, 0);
    }

    #[test]
    fn test_message_priority_ordering() {
        let mut msg1 = OfflineMessage::new(
            1,
            "u1".to_string(),
            "u2".to_string(),
            "c1".to_string(),
            Bytes::from("1"),
            "text".to_string(),
        );
        let mut msg2 = OfflineMessage::new(
            2,
            "u1".to_string(),
            "u2".to_string(),
            "c1".to_string(),
            Bytes::from("2"),
            "text".to_string(),
        );

        msg1.priority = MessagePriority::Low;
        msg2.priority = MessagePriority::High;

        assert!(msg2 < msg1); // 高优先级应该排在前面
    }

    #[test]
    fn test_retry_logic() {
        let mut message = OfflineMessage::new(
            1,
            "u1".to_string(),
            "u2".to_string(),
            "c1".to_string(),
            Bytes::from("test"),
            "text".to_string(),
        );

        assert!(message.can_retry());
        assert!(message.mark_failed_for_retry());
        assert_eq!(message.retry_count, 1);
        assert_eq!(message.status, DeliveryStatus::Pending);

        // 重试到最大次数
        message.mark_failed_for_retry();
        message.mark_failed_for_retry();
        assert!(!message.mark_failed_for_retry());
        assert_eq!(message.status, DeliveryStatus::Failed);
    }

    #[test]
    fn test_expiry() {
        let mut message = OfflineMessage::new(
            1,
            "u1".to_string(),
            "u2".to_string(),
            "c1".to_string(),
            Bytes::from("test"),
            "text".to_string(),
        );

        // 设置1秒后过期
        message = message.with_expiry(chrono::Duration::seconds(-1));
        assert!(message.is_expired());
    }
}
