// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::error::{Result, ServerError};
use crate::model::channel::{MessageId, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 消息 Reaction（点赞/表情）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    /// 消息ID
    pub message_id: MessageId,
    /// 用户ID
    pub user_id: UserId,
    /// Emoji 表情（如 👍, ❤️, 😂, 😮, 😢）
    pub emoji: String,
    /// 创建时间
    pub created_at: DateTime<Utc>,
}

/// Reaction 统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionStats {
    /// Emoji -> 用户ID列表
    pub reactions: HashMap<String, Vec<UserId>>,
    /// 总 Reaction 数量
    pub total_count: usize,
}

/// Reaction 服务
pub struct ReactionService {
    /// message_id -> { user_id -> Reaction }
    reactions: Arc<RwLock<HashMap<MessageId, HashMap<UserId, Reaction>>>>,
}

impl ReactionService {
    /// 创建新的 Reaction 服务
    pub fn new() -> Self {
        Self {
            reactions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 添加 Reaction
    pub async fn add_reaction(
        &self,
        message_id: u64,
        user_id: u64,
        emoji: &str,
    ) -> Result<Reaction> {
        // 验证 emoji 格式（简单验证，实际应该更严格）
        if emoji.is_empty() || emoji.len() > 10 {
            return Err(ServerError::Validation("无效的 emoji".to_string()));
        }

        let reaction = Reaction {
            message_id,
            user_id,
            emoji: emoji.to_string(),
            created_at: Utc::now(),
        };

        let mut reactions = self.reactions.write().await;
        let message_reactions = reactions.entry(message_id).or_insert_with(HashMap::new);

        // 如果用户已经对该消息有 Reaction，则替换（一个用户只能有一个 Reaction）
        message_reactions.insert(user_id, reaction.clone());

        Ok(reaction)
    }

    /// 移除 Reaction
    pub async fn remove_reaction(&self, message_id: u64, user_id: u64) -> Result<()> {
        let mut reactions = self.reactions.write().await;

        if let Some(message_reactions) = reactions.get_mut(&message_id) {
            message_reactions.remove(&user_id);

            // 如果该消息没有 Reaction 了，删除整个条目
            if message_reactions.is_empty() {
                reactions.remove(&message_id);
            }
        }

        Ok(())
    }

    /// 获取消息的所有 Reaction
    pub async fn get_message_reactions(&self, message_id: u64) -> Result<ReactionStats> {
        let reactions = self.reactions.read().await;

        if let Some(message_reactions) = reactions.get(&message_id) {
            // 按 emoji 分组
            let mut emoji_to_users: HashMap<String, Vec<UserId>> = HashMap::new();

            for (user_id, reaction) in message_reactions.iter() {
                emoji_to_users
                    .entry(reaction.emoji.clone())
                    .or_insert_with(Vec::new)
                    .push(user_id.clone());
            }

            let total_count = message_reactions.len();

            Ok(ReactionStats {
                reactions: emoji_to_users,
                total_count,
            })
        } else {
            Ok(ReactionStats {
                reactions: HashMap::new(),
                total_count: 0,
            })
        }
    }

    /// 检查用户是否对消息有 Reaction
    pub async fn has_reaction(&self, message_id: u64, user_id: u64) -> Option<Reaction> {
        let reactions = self.reactions.read().await;

        reactions.get(&message_id)?.get(&user_id).cloned()
    }

    /// 获取用户对消息的 Reaction（如果存在）
    pub async fn get_user_reaction(&self, message_id: u64, user_id: u64) -> Option<Reaction> {
        self.has_reaction(message_id, user_id).await
    }
}

impl Default for ReactionService {
    fn default() -> Self {
        Self::new()
    }
}
