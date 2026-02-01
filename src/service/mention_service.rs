use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use crate::model::channel::{MessageId, UserId, ChannelId};
use crate::error::Result;

/// @提及记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    /// 消息ID
    pub message_id: MessageId,
    /// 频道ID
    pub channel_id: ChannelId,
    /// 被@的用户ID列表
    pub mentioned_user_ids: Vec<UserId>,
    /// 是否@全体成员
    pub is_mention_all: bool,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// @提及服务
pub struct MentionService {
    /// message_id -> Mention
    mentions: Arc<RwLock<HashMap<MessageId, Mention>>>,
    /// user_id -> [message_id]（用户被@的消息列表）
    user_mentions: Arc<RwLock<HashMap<UserId, Vec<MessageId>>>>,
}

impl MentionService {
    /// 创建新的 @提及服务
    pub fn new() -> Self {
        Self {
            mentions: Arc::new(RwLock::new(HashMap::new())),
            user_mentions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 解析消息中的@提及
    /// 
    /// 支持格式：
    /// - @username
    /// - @全体成员（需要权限检查）
    pub fn parse_mentions(content: &str) -> (Vec<String>, bool) {
        let mut mentioned_usernames = Vec::new();
        let mut is_mention_all = false;
        
        // 简单的@解析（实际应该更智能，支持中文用户名等）
        let words: Vec<&str> = content.split_whitespace().collect();
        
        for word in words {
            if word.starts_with("@") {
                let username = word.strip_prefix("@").unwrap_or("");
                
                // 检查是否是@全体成员
                if username == "全体成员" || username == "all" || username == "everyone" {
                    is_mention_all = true;
                } else if !username.is_empty() {
                    mentioned_usernames.push(username.to_string());
                }
            }
        }
        
        (mentioned_usernames, is_mention_all)
    }

    /// 记录@提及
    pub async fn record_mention(
        &self,
        message_id: u64,
        channel_id: u64,
        mentioned_user_ids: Vec<u64>,
        is_mention_all: bool,
    ) -> Result<Mention> {
        let mention = Mention {
            message_id,
            channel_id,
            mentioned_user_ids: mentioned_user_ids.clone(),
            is_mention_all,
            created_at: Utc::now(),
        };

        let mut mentions = self.mentions.write().await;
        mentions.insert(message_id, mention.clone());

        // 更新用户被@的索引
        let mut user_mentions = self.user_mentions.write().await;
        for user_id in mentioned_user_ids {
            user_mentions
                .entry(user_id)
                .or_insert_with(Vec::new)
                .push(message_id);
        }

        Ok(mention)
    }

    /// 获取消息的@提及信息
    pub async fn get_message_mentions(
        &self,
        message_id: u64,
    ) -> Option<Mention> {
        let mentions = self.mentions.read().await;
        mentions.get(&message_id).cloned()
    }

    /// 获取用户被@的消息列表
    pub async fn get_user_mentions(
        &self,
        user_id: u64,
        limit: Option<usize>,
    ) -> Vec<MessageId> {
        let user_mentions = self.user_mentions.read().await;
        
        if let Some(message_ids) = user_mentions.get(&user_id) {
            let mut result = message_ids.clone();
            result.reverse(); // 最新的在前
            
            if let Some(limit) = limit {
                result.truncate(limit);
            }
            
            result
        } else {
            Vec::new()
        }
    }

    /// 检查消息是否@了指定用户
    pub async fn is_user_mentioned(
        &self,
        message_id: u64,
        user_id: u64,
    ) -> bool {
        let mentions = self.mentions.read().await;
        
        if let Some(mention) = mentions.get(&message_id) {
            mention.mentioned_user_ids.contains(&user_id)
        } else {
            false
        }
    }
}

impl Default for MentionService {
    fn default() -> Self {
        Self::new()
    }
}
