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
use sqlx::{FromRow, PgPool};
use std::collections::HashMap;
use std::sync::Arc;

/// 消息 Reaction（点赞/表情）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reaction {
    pub message_id: MessageId,
    pub user_id: UserId,
    pub emoji: String,
    pub created_at: DateTime<Utc>,
}

/// Reaction 统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactionStats {
    pub reactions: HashMap<String, Vec<UserId>>,
    pub total_count: usize,
}

#[derive(Debug, FromRow)]
struct ReactionRow {
    emoji: String,
    created_at: i64,
}

#[derive(Debug, FromRow)]
struct ReactionStatsRow {
    user_id: i64,
    emoji: String,
}

/// Reaction 服务
pub struct ReactionService {
    pool: Arc<PgPool>,
}

impl ReactionService {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 添加 Reaction
    pub async fn add_reaction(
        &self,
        message_id: u64,
        user_id: u64,
        emoji: &str,
    ) -> Result<Reaction> {
        if emoji.is_empty() || emoji.len() > 10 {
            return Err(ServerError::Validation("无效的 emoji".to_string()));
        }

        let created_at_ms = Utc::now().timestamp_millis();
        let row = sqlx::query_as::<_, ReactionRow>(
            r#"
            INSERT INTO privchat_message_reactions
                (message_id, user_id, emoji, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (message_id, user_id)
            DO UPDATE SET
                emoji = EXCLUDED.emoji,
                updated_at = EXCLUDED.updated_at
            RETURNING emoji, created_at
            "#,
        )
        .bind(message_id as i64)
        .bind(user_id as i64)
        .bind(emoji)
        .bind(created_at_ms)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("添加 Reaction 失败: {}", e)))?;

        Ok(Reaction {
            message_id,
            user_id,
            emoji: emoji.to_string(),
            created_at: DateTime::from_timestamp_millis(row.created_at).unwrap_or_else(Utc::now),
        })
    }

    /// 移除 Reaction
    pub async fn remove_reaction(&self, message_id: u64, user_id: u64) -> Result<Option<Reaction>> {
        let removed = self.get_user_reaction(message_id, user_id).await;
        sqlx::query(
            r#"
            DELETE FROM privchat_message_reactions
            WHERE message_id = $1 AND user_id = $2
            "#,
        )
        .bind(message_id as i64)
        .bind(user_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("移除 Reaction 失败: {}", e)))?;

        Ok(removed)
    }

    /// 获取消息的所有 Reaction
    pub async fn get_message_reactions(&self, message_id: u64) -> Result<ReactionStats> {
        let rows = sqlx::query_as::<_, ReactionStatsRow>(
            r#"
            SELECT user_id, emoji
            FROM privchat_message_reactions
            WHERE message_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(message_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询 Reaction 失败: {}", e)))?;

        let mut reactions: HashMap<String, Vec<UserId>> = HashMap::new();
        for row in rows {
            reactions
                .entry(row.emoji)
                .or_default()
                .push(row.user_id as u64);
        }
        let total_count = reactions.values().map(Vec::len).sum();

        Ok(ReactionStats {
            reactions,
            total_count,
        })
    }

    /// 检查用户是否对消息有 Reaction
    pub async fn has_reaction(&self, message_id: u64, user_id: u64) -> Option<Reaction> {
        self.get_user_reaction(message_id, user_id).await
    }

    /// 获取用户对消息的 Reaction（如果存在）
    pub async fn get_user_reaction(&self, message_id: u64, user_id: u64) -> Option<Reaction> {
        let row = sqlx::query_as::<_, ReactionRow>(
            r#"
            SELECT emoji, created_at
            FROM privchat_message_reactions
            WHERE message_id = $1 AND user_id = $2
            "#,
        )
        .bind(message_id as i64)
        .bind(user_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .ok()
        .flatten()?;

        Some(Reaction {
            message_id,
            user_id,
            emoji: row.emoji,
            created_at: DateTime::from_timestamp_millis(row.created_at).unwrap_or_else(Utc::now),
        })
    }
}
