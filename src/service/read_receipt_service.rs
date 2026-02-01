use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::channel::{UserId, MessageId, ChannelId};
use crate::error::Result;

/// 已读回执记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipt {
    /// 消息ID
    pub message_id: MessageId,
    /// 频道ID
    pub channel_id: ChannelId,
    /// 阅读者ID
    pub user_id: UserId,
    /// 阅读时间
    pub read_at: DateTime<Utc>,
}

/// 群消息已读统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupReadStats {
    /// 消息ID
    pub message_id: MessageId,
    /// 频道ID
    pub channel_id: ChannelId,
    /// 群成员总数
    pub total_members: u32,
    /// 已读人数
    pub read_count: u32,
    /// 已读用户列表（可选，用于性能优化）
    pub read_users: Option<Vec<UserId>>,
}

/// 已读回执服务
pub struct ReadReceiptService {
    /// 已读回执存储 - (message_id, user_id) -> ReadReceipt
    read_receipts: Arc<RwLock<HashMap<(MessageId, UserId), ReadReceipt>>>,
    /// 消息已读索引 - message_id -> Vec<user_id>
    message_readers: Arc<RwLock<HashMap<MessageId, Vec<UserId>>>>,
}

impl ReadReceiptService {
    /// 创建新的已读回执服务
    pub fn new() -> Self {
        Self {
            read_receipts: Arc::new(RwLock::new(HashMap::new())),
            message_readers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 记录已读回执
    pub async fn record_read_receipt(
        &self,
        message_id: MessageId,
        channel_id: ChannelId,
        user_id: UserId,
    ) -> Result<ReadReceipt> {
        let read_at = Utc::now();
        
        let receipt = ReadReceipt {
            message_id: message_id.clone(),
            channel_id: channel_id.clone(),
            user_id: user_id.clone(),
            read_at,
        };

        // 存储已读回执
        let mut read_receipts = self.read_receipts.write().await;
        read_receipts.insert((message_id.clone(), user_id.clone()), receipt.clone());

        // 更新消息已读索引
        let mut message_readers = self.message_readers.write().await;
        let readers = message_readers.entry(message_id).or_insert_with(Vec::new);
        if !readers.contains(&user_id) {
            readers.push(user_id);
        }

        Ok(receipt)
    }

    /// 查询消息是否已读（私聊）
    pub async fn is_message_read(
        &self,
        message_id: &MessageId,
        user_id: &UserId,
    ) -> Result<bool> {
        let read_receipts = self.read_receipts.read().await;
        Ok(read_receipts.contains_key(&(message_id.clone(), user_id.clone())))
    }

    /// 获取消息的已读时间（私聊）
    pub async fn get_read_time(
        &self,
        message_id: &MessageId,
        user_id: &UserId,
    ) -> Result<Option<DateTime<Utc>>> {
        let read_receipts = self.read_receipts.read().await;
        Ok(read_receipts
            .get(&(message_id.clone(), user_id.clone()))
            .map(|r| r.read_at))
    }

    /// 获取群消息已读列表
    pub async fn get_group_read_list(
        &self,
        message_id: &MessageId,
        channel_id: &ChannelId,
        total_members: u32,
    ) -> Result<GroupReadStats> {
        let message_readers = self.message_readers.read().await;
        let readers = message_readers
            .get(message_id)
            .cloned()
            .unwrap_or_default();

        Ok(GroupReadStats {
            message_id: message_id.clone(),
            channel_id: channel_id.clone(),
            total_members,
            read_count: readers.len() as u32,
            read_users: Some(readers),
        })
    }

    /// 获取群消息已读统计（不包含用户列表，性能更好）
    pub async fn get_group_read_stats(
        &self,
        message_id: &MessageId,
        channel_id: &ChannelId,
        total_members: u32,
    ) -> Result<GroupReadStats> {
        let message_readers = self.message_readers.read().await;
        let read_count = message_readers
            .get(message_id)
            .map(|readers| readers.len() as u32)
            .unwrap_or(0);

        Ok(GroupReadStats {
            message_id: message_id.clone(),
            channel_id: channel_id.clone(),
            total_members,
            read_count,
            read_users: None, // 不包含用户列表，性能更好
        })
    }

    /// 批量记录已读回执
    pub async fn batch_record_read_receipts(
        &self,
        message_ids: &[MessageId],
        channel_id: ChannelId,
        user_id: UserId,
    ) -> Result<Vec<ReadReceipt>> {
        let mut receipts = Vec::new();
        let read_at = Utc::now();

        // 批量存储
        {
            let mut read_receipts = self.read_receipts.write().await;
            let mut message_readers = self.message_readers.write().await;

            for message_id in message_ids {
                let receipt = ReadReceipt {
                    message_id: message_id.clone(),
                    channel_id: channel_id.clone(),
                    user_id: user_id.clone(),
                    read_at,
                };

                read_receipts.insert((message_id.clone(), user_id.clone()), receipt.clone());
                receipts.push(receipt);

                // 更新消息已读索引
                let readers = message_readers.entry(message_id.clone()).or_insert_with(Vec::new);
                if !readers.contains(&user_id) {
                    readers.push(user_id.clone());
                }
            }
        }

        Ok(receipts)
    }
}

