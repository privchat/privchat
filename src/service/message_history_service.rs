use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::error::{Result, ServerError};
use crate::model::channel::{ChannelId, MessageId, UserId};

/// 引用消息预览（用于消息回复）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyMessagePreview {
    /// 被引用的消息ID
    pub message_id: MessageId,
    /// 被引用消息的发送者ID
    pub sender_id: UserId,
    /// 被引用消息的内容预览（最多50字符）
    pub content: String,
    /// 被引用消息的类型
    pub message_type: u32,
}

/// 消息历史记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHistoryRecord {
    /// 消息ID
    pub message_id: MessageId,
    /// 频道ID
    pub channel_id: ChannelId,
    /// 发送者ID
    pub sender_id: UserId,
    /// 消息内容
    pub content: String,
    /// 消息类型
    pub message_type: u32,
    /// 消息序号
    pub seq: u64,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 更新时间
    pub updated_at: DateTime<Utc>,
    /// 是否已删除
    pub is_deleted: bool,
    /// 删除时间
    pub deleted_at: Option<DateTime<Utc>>,
    /// 是否已撤回
    pub is_revoked: bool,
    /// 撤回时间
    pub revoked_at: Option<DateTime<Utc>>,
    /// 撤回者ID（可能是发送者本人或管理员）
    pub revoker_id: Option<UserId>,
    /// 回复的消息ID
    pub reply_to_message_id: Option<MessageId>,
    /// 消息元数据
    pub metadata: Option<String>,
}

/// 消息查询参数
#[derive(Debug, Clone)]
pub struct MessageQueryParams {
    /// 频道ID
    pub channel_id: ChannelId,
    /// 起始消息ID（不包含）
    pub before_message_id: Option<MessageId>,
    /// 结束消息ID（不包含）
    pub after_message_id: Option<MessageId>,
    /// 限制数量
    pub limit: u32,
    /// 是否包含已删除消息
    pub include_deleted: bool,
    /// 消息类型过滤
    pub message_types: Option<Vec<u32>>,
    /// 发送者过滤
    pub sender_ids: Option<Vec<UserId>>,
    /// 时间范围过滤
    pub start_time: Option<DateTime<Utc>>,
    pub end_time: Option<DateTime<Utc>>,
}

impl Default for MessageQueryParams {
    fn default() -> Self {
        Self {
            channel_id: 0,
            before_message_id: None,
            after_message_id: None,
            limit: 50,
            include_deleted: false,
            message_types: None,
            sender_ids: None,
            start_time: None,
            end_time: None,
        }
    }
}

/// 消息历史服务
///
/// 性能优化：
/// - 使用 BTreeMap 按 seq 排序，支持高效的范围查询
/// - 使用 DashMap 替代 Arc<RwLock<HashMap>>，减少锁竞争
/// - 支持批量操作，为持久化做准备
pub struct MessageHistoryService {
    /// 频道消息历史存储 - 频道ID -> BTreeMap<seq, MessageHistoryRecord>
    /// 使用 BTreeMap 按 seq 排序，支持高效的范围查询和排序
    channel_messages: DashMap<ChannelId, BTreeMap<u64, MessageHistoryRecord>>,
    /// 消息ID索引 - 消息ID -> (频道ID, seq)
    message_index: DashMap<MessageId, (ChannelId, u64)>,
    /// 消息序号生成器 - 频道ID -> 当前序号（使用 AtomicU64 优化）
    seq_generators: DashMap<ChannelId, std::sync::atomic::AtomicU64>,
    /// 每个频道最大消息数量
    max_messages_per_channel: usize,
}

impl MessageHistoryService {
    /// 创建新的消息历史服务
    pub fn new(max_messages_per_channel: usize) -> Self {
        Self {
            channel_messages: DashMap::new(),
            message_index: DashMap::new(),
            seq_generators: DashMap::new(),
            max_messages_per_channel,
        }
    }

    /// 存储消息
    pub async fn store_message(
        &self,
        channel_id: &ChannelId,
        sender_id: &UserId,
        content: String,
        message_type: u32,
        reply_to_message_id: Option<MessageId>,
        metadata: Option<String>,
    ) -> Result<MessageHistoryRecord> {
        // 使用 Snowflake 生成消息ID
        use crate::infra::next_message_id;
        let message_id = next_message_id();

        // 获取下一个序号（使用原子操作，无需锁）
        let seq = {
            let counter = self
                .seq_generators
                .entry(*channel_id)
                .or_insert_with(|| std::sync::atomic::AtomicU64::new(0));
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
        };

        let record = MessageHistoryRecord {
            message_id,
            channel_id: *channel_id,
            sender_id: *sender_id,
            content,
            message_type,
            seq,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_deleted: false,
            deleted_at: None,
            is_revoked: false,
            revoked_at: None,
            revoker_id: None,
            reply_to_message_id,
            metadata,
        };

        // 存储消息（使用 DashMap，无需显式锁）
        let mut messages = self
            .channel_messages
            .entry(*channel_id)
            .or_insert_with(BTreeMap::new);

        // 如果超过最大消息数量，删除最旧的消息（BTreeMap 按 seq 排序，第一个是最旧的）
        if messages.len() >= self.max_messages_per_channel {
            if let Some((_, old_message)) = messages.pop_first() {
                // 更新索引
                self.message_index.remove(&old_message.message_id);
            }
        }

        // 添加新消息（BTreeMap 自动按 seq 排序）
        messages.insert(seq, record.clone());

        // 更新索引
        self.message_index.insert(message_id, (*channel_id, seq));

        Ok(record)
    }

    /// 查询消息历史
    ///
    /// 性能优化：
    /// - 使用 BTreeMap 的范围查询，避免全量遍历
    /// - 支持按 seq 范围查询，高效获取指定范围的消息
    pub async fn query_messages(
        &self,
        params: MessageQueryParams,
    ) -> Result<Vec<MessageHistoryRecord>> {
        let messages = self
            .channel_messages
            .get(&params.channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        // 确定查询范围（如果指定了 before_message_id 或 after_message_id）
        let (start_seq, end_seq) = if let Some(ref before_id) = params.before_message_id {
            // 找到 before_message_id 对应的 seq
            if let Some(entry) = self.message_index.get(before_id) {
                (0, entry.value().1) // 查询 before_seq 之前的消息
            } else {
                (0, u64::MAX) // 如果找不到，查询所有
            }
        } else if let Some(ref after_id) = params.after_message_id {
            // 找到 after_message_id 对应的 seq
            if let Some(entry) = self.message_index.get(after_id) {
                (entry.value().1 + 1, u64::MAX) // 查询 after_seq 之后的消息
            } else {
                (0, u64::MAX) // 如果找不到，查询所有
            }
        } else {
            (0, u64::MAX) // 查询所有
        };

        // 使用 BTreeMap 的范围查询，高效获取指定范围的消息
        let filtered_messages: Vec<MessageHistoryRecord> = messages
            .range(start_seq..=end_seq)
            .rev() // 倒序（最新的在前）
            .map(|(_, msg)| msg.clone())
            .filter(|msg| {
                // 过滤已删除消息
                if !params.include_deleted && msg.is_deleted {
                    return false;
                }

                // 过滤消息类型
                if let Some(ref types) = params.message_types {
                    if !types.contains(&msg.message_type) {
                        return false;
                    }
                }

                // 过滤发送者
                if let Some(ref sender_ids) = params.sender_ids {
                    if !sender_ids.contains(&msg.sender_id) {
                        return false;
                    }
                }

                // 过滤时间范围
                if let Some(start_time) = params.start_time {
                    if msg.created_at < start_time {
                        return false;
                    }
                }

                if let Some(end_time) = params.end_time {
                    if msg.created_at > end_time {
                        return false;
                    }
                }

                true
            })
            .take(params.limit as usize)
            .collect();

        Ok(filtered_messages)
    }

    /// 获取消息详情
    ///
    /// 支持两种格式的 message_id：
    /// 1. UUID 格式的字符串（如 "msg_xxx"）
    /// 2. 数字字符串（seq，如 "123"），需要通过频道ID和seq查找
    pub async fn get_message(&self, message_id: &MessageId) -> Result<MessageHistoryRecord> {
        // 通过 message_index 查找 (channel_id, seq)
        let (channel_id, seq) = self
            .message_index
            .get(message_id)
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))?
            .clone();

        // 通过 channel_id 和 seq 从 BTreeMap 中获取消息
        let messages = self
            .channel_messages
            .get(&channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        messages
            .get(&seq)
            .cloned()
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))
    }

    /// 通过频道ID和序号查找消息
    pub async fn get_message_by_seq(
        &self,
        channel_id: &ChannelId,
        seq: u64,
    ) -> Result<MessageHistoryRecord> {
        let messages = self
            .channel_messages
            .get(channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        messages
            .get(&seq)
            .cloned()
            .ok_or_else(|| ServerError::NotFound("Message not found by seq".to_string()))
    }

    /// 删除消息
    pub async fn delete_message(&self, message_id: &MessageId, user_id: &UserId) -> Result<()> {
        let (channel_id, seq) = self
            .message_index
            .get(message_id)
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))?
            .clone();

        let mut messages = self
            .channel_messages
            .get_mut(&channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        let message = messages
            .get_mut(&seq)
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))?;

        // 只有发送者可以删除消息
        if message.sender_id != *user_id {
            return Err(ServerError::Forbidden(
                "Only sender can delete message".to_string(),
            ));
        }

        message.is_deleted = true;
        message.deleted_at = Some(Utc::now());
        message.updated_at = Utc::now();

        Ok(())
    }

    /// 更新消息内容
    pub async fn update_message(
        &self,
        message_id: &MessageId,
        user_id: &UserId,
        new_content: String,
        metadata: Option<String>,
    ) -> Result<()> {
        let (channel_id, seq) = self
            .message_index
            .get(message_id)
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))?
            .clone();

        let mut messages = self
            .channel_messages
            .get_mut(&channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        let message = messages
            .get_mut(&seq)
            .ok_or_else(|| ServerError::NotFound("Message not found".to_string()))?;

        // 只有发送者可以编辑消息
        if message.sender_id != *user_id {
            return Err(ServerError::Forbidden(
                "Only sender can edit message".to_string(),
            ));
        }

        message.content = new_content;
        message.updated_at = Utc::now();

        if let Some(metadata) = metadata {
            message.metadata = Some(metadata);
        }

        Ok(())
    }

    /// 获取频道消息统计
    pub async fn get_channel_message_stats(
        &self,
        channel_id: &ChannelId,
    ) -> Result<ChannelMessageStats> {
        let messages = self
            .channel_messages
            .get(channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        let total_messages = messages.len() as u64;
        let deleted_messages = messages.values().filter(|msg| msg.is_deleted).count() as u64;
        let active_messages = total_messages - deleted_messages;

        // 统计消息类型分布
        let mut message_type_counts = HashMap::new();
        for msg in messages.values() {
            if !msg.is_deleted {
                *message_type_counts.entry(msg.message_type).or_insert(0) += 1;
            }
        }

        // 统计发送者分布
        let mut sender_counts = HashMap::new();
        for msg in messages.values() {
            if !msg.is_deleted {
                *sender_counts.entry(msg.sender_id).or_insert(0) += 1;
            }
        }

        // 获取最后一条消息（BTreeMap 最后一个元素是最新的）
        let last_message = messages
            .iter()
            .rev()
            .find(|(_, msg)| !msg.is_deleted)
            .map(|(_, msg)| msg.clone());

        Ok(ChannelMessageStats {
            channel_id: *channel_id,
            total_messages,
            active_messages,
            deleted_messages,
            message_type_counts,
            sender_counts,
            last_message,
            stats_time: Utc::now(),
        })
    }

    /// 清理频道消息历史
    pub async fn clear_channel_messages(&self, channel_id: &ChannelId) -> Result<u64> {
        let messages = self
            .channel_messages
            .remove(channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        let count = messages.1.len() as u64;

        // 清理索引（批量删除，提高性能）
        for (_, msg) in messages.1 {
            self.message_index.remove(&msg.message_id);
        }

        // 重置序号生成器
        self.seq_generators.remove(channel_id);

        Ok(count)
    }

    /// 获取频道最新消息
    pub async fn get_latest_messages(
        &self,
        channel_id: &ChannelId,
        limit: u32,
    ) -> Result<Vec<MessageHistoryRecord>> {
        let params = MessageQueryParams {
            channel_id: *channel_id,
            limit,
            ..Default::default()
        };

        self.query_messages(params).await
    }

    /// 撤回消息
    ///
    /// # 参数
    /// - `message_id`: 消息ID
    /// - `revoker_id`: 撤回者ID
    ///
    /// # 返回
    /// - 成功返回撤回后的消息记录
    pub async fn revoke_message(
        &self,
        message_id: &MessageId,
        revoker_id: &UserId,
    ) -> Result<MessageHistoryRecord> {
        let (channel_id, seq) = self
            .message_index
            .get(message_id)
            .ok_or_else(|| ServerError::NotFound("消息不存在".to_string()))?
            .clone();

        let mut messages = self
            .channel_messages
            .get_mut(&channel_id)
            .ok_or_else(|| ServerError::NotFound("频道不存在".to_string()))?;

        let message = messages
            .get_mut(&seq)
            .ok_or_else(|| ServerError::NotFound("消息不存在".to_string()))?;

        // 检查是否已撤回
        if message.is_revoked {
            return Err(ServerError::InvalidRequest("消息已被撤回".to_string()));
        }

        // 检查是否已删除
        if message.is_deleted {
            return Err(ServerError::InvalidRequest("消息已被删除".to_string()));
        }

        // 标记为已撤回
        message.is_revoked = true;
        message.revoked_at = Some(Utc::now());
        message.revoker_id = Some(revoker_id.clone());
        message.updated_at = Utc::now();

        Ok(message.clone())
    }

    /// 批量存储消息（性能优化：为持久化做准备）
    ///
    /// 批量操作可以减少锁竞争和数据库写入次数
    pub async fn store_messages_batch(&self, records: Vec<MessageHistoryRecord>) -> Result<usize> {
        let mut count = 0;

        // 按频道分组，减少 DashMap 的访问次数
        let mut by_channel: HashMap<ChannelId, Vec<MessageHistoryRecord>> = HashMap::new();
        for record in records {
            by_channel
                .entry(record.channel_id.clone())
                .or_insert_with(Vec::new)
                .push(record);
        }

        // 批量处理每个频道的消息
        for (channel_id, channel_records) in by_channel {
            let mut messages = self
                .channel_messages
                .entry(channel_id.clone())
                .or_insert_with(BTreeMap::new);

            for record in channel_records {
                // 如果超过最大消息数量，删除最旧的消息
                if messages.len() >= self.max_messages_per_channel {
                    if let Some((_, old_message)) = messages.pop_first() {
                        self.message_index.remove(&old_message.message_id);
                    }
                }

                // 添加新消息
                messages.insert(record.seq, record.clone());
                self.message_index
                    .insert(record.message_id.clone(), (channel_id.clone(), record.seq));
                count += 1;
            }
        }

        Ok(count)
    }

    /// 批量查询消息（性能优化：支持范围查询）
    ///
    /// 通过 seq 范围查询，高效获取指定范围的消息
    pub async fn query_messages_by_seq_range(
        &self,
        channel_id: &ChannelId,
        start_seq: u64,
        end_seq: u64,
        limit: u32,
    ) -> Result<Vec<MessageHistoryRecord>> {
        let messages = self
            .channel_messages
            .get(channel_id)
            .ok_or_else(|| ServerError::NotFound("Channel not found".to_string()))?;

        // 使用 BTreeMap 的范围查询，高效获取指定范围的消息
        let result: Vec<MessageHistoryRecord> = messages
            .range(start_seq..=end_seq)
            .rev() // 倒序（最新的在前）
            .take(limit as usize)
            .map(|(_, msg)| msg.clone())
            .collect();

        Ok(result)
    }
}

/// 频道消息统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelMessageStats {
    /// 频道ID
    pub channel_id: ChannelId,
    /// 总消息数
    pub total_messages: u64,
    /// 活跃消息数（未删除）
    pub active_messages: u64,
    /// 已删除消息数
    pub deleted_messages: u64,
    /// 消息类型分布
    pub message_type_counts: HashMap<u32, u64>,
    /// 发送者分布
    pub sender_counts: HashMap<UserId, u64>,
    /// 最后一条消息
    pub last_message: Option<MessageHistoryRecord>,
    /// 统计时间
    pub stats_time: DateTime<Utc>,
}
