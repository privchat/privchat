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

use crate::error::Result;
use crate::infra::database::Database;
use privchat_protocol::rpc::sync::ServerCommit;
use sqlx::Row;
/// Commit Log 数据库操作
///
/// 负责 privchat_commit_log 表的 CRUD 操作
use std::sync::Arc;
use tracing::{debug, error};

/// Commit Log DAO
pub struct CommitLogDao {
    db: Arc<Database>,
}

impl CommitLogDao {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// 保存 Commit 到数据库
    ///
    /// INSERT INTO privchat_commit_log (...)
    pub async fn save_commit(&self, commit: &ServerCommit) -> Result<()> {
        debug!(
            "保存 Commit: channel_id={}, channel_type={}, pts={}, server_msg_id={}",
            commit.channel_id, commit.channel_type, commit.pts, commit.server_msg_id
        );

        let now = chrono::Utc::now().timestamp_millis();
        let sender_username = commit.sender_info.as_ref().map(|s| s.username.as_str());

        sqlx::query!(
            r#"
            INSERT INTO privchat_commit_log 
            (pts, server_msg_id, local_message_id, channel_id, channel_type, 
             message_type, content, server_timestamp, sender_id, sender_username, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
            commit.pts as i64,
            commit.server_msg_id as i64,
            commit.local_message_id.map(|v| v as i64),
            commit.channel_id as i64,
            commit.channel_type as i16,
            commit.message_type.as_str(),
            commit.content.clone(),
            commit.server_timestamp,
            commit.sender_id as i64,
            sender_username,
            now
        )
        .execute(self.db.pool())
        .await
        .map_err(|e| {
            error!(
                "保存 Commit 失败: channel_id={}, pts={}, error={}",
                commit.channel_id, commit.pts, e
            );
            crate::error::ServerError::Database(format!("Failed to save commit: {}", e))
        })?;

        Ok(())
    }

    /// 在单个事务内完成「分配 pts + 写入 commit」
    ///
    /// 目的：避免先分配 pts 再写 commit 失败导致 pts 空洞。
    /// 成功后会回写 `commit.pts`。
    pub async fn allocate_pts_and_save_commit(&self, commit: &mut ServerCommit) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let sender_username = commit.sender_info.as_ref().map(|s| s.username.as_str());

        let mut tx = self.db.pool().begin().await.map_err(|e| {
            error!(
                "开启事务失败: channel_id={}, error={}",
                commit.channel_id, e
            );
            crate::error::ServerError::Database(format!(
                "Failed to begin commit transaction: {}",
                e
            ))
        })?;

        let pts_row = sqlx::query(
            r#"
            INSERT INTO privchat_channel_pts
            (channel_id, current_pts, created_at, updated_at)
            VALUES ($1, 1, $2, $2)
            ON CONFLICT (channel_id) DO UPDATE
            SET current_pts = privchat_channel_pts.current_pts + 1,
                updated_at = $2
            RETURNING current_pts
            "#,
        )
        .bind(commit.channel_id as i64)
        .bind(now)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| {
            error!(
                "事务内分配 pts 失败: channel_id={}, error={}",
                commit.channel_id, e
            );
            crate::error::ServerError::Database(format!(
                "Failed to allocate pts in transaction: {}",
                e
            ))
        })?;

        let new_pts = pts_row.get::<i64, _>("current_pts") as u64;
        commit.pts = new_pts;

        sqlx::query!(
            r#"
            INSERT INTO privchat_commit_log
            (pts, server_msg_id, local_message_id, channel_id, channel_type,
             message_type, content, server_timestamp, sender_id, sender_username, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            "#,
            commit.pts as i64,
            commit.server_msg_id as i64,
            commit.local_message_id.map(|v| v as i64),
            commit.channel_id as i64,
            commit.channel_type as i16,
            commit.message_type.as_str(),
            commit.content.clone(),
            commit.server_timestamp,
            commit.sender_id as i64,
            sender_username,
            now
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            error!(
                "事务内保存 Commit 失败: channel_id={}, pts={}, error={}",
                commit.channel_id, commit.pts, e
            );
            crate::error::ServerError::Database(format!(
                "Failed to save commit in transaction: {}",
                e
            ))
        })?;

        tx.commit().await.map_err(|e| {
            error!(
                "提交事务失败: channel_id={}, pts={}, error={}",
                commit.channel_id, commit.pts, e
            );
            crate::error::ServerError::Database(format!(
                "Failed to commit commit transaction: {}",
                e
            ))
        })?;

        Ok(())
    }

    /// 查询 Commits（pts > last_pts）
    ///
    /// SELECT * FROM privchat_commit_log
    /// WHERE channel_id = ? AND channel_type = ? AND pts > ?
    /// ORDER BY pts ASC
    /// LIMIT ?
    pub async fn query_commits(
        &self,
        channel_id: u64,
        last_pts: u64,
        limit: u32,
    ) -> Result<Vec<ServerCommit>> {
        use privchat_protocol::rpc::sync::SenderInfo;

        debug!(
            "查询 Commits: channel_id={}, last_pts={}, limit={}",
            channel_id, last_pts, limit
        );

        #[derive(sqlx::FromRow)]
        struct CommitRow {
            pts: i64,
            server_msg_id: i64,
            local_message_id: Option<i64>,
            channel_id: i64,
            channel_type: i16,
            message_type: String,
            content: serde_json::Value,
            server_timestamp: i64,
            sender_id: i64,
            sender_username: Option<String>,
        }

        let rows = sqlx::query_as::<_, CommitRow>(
            r#"
            SELECT 
                pts, server_msg_id, local_message_id, channel_id, channel_type,
                message_type, content, server_timestamp, sender_id, sender_username
            FROM privchat_commit_log
            WHERE channel_id = $1 AND pts > $2
            ORDER BY pts ASC
            LIMIT $3
            "#,
        )
        .bind(channel_id as i64)
        .bind(last_pts as i64)
        .bind(limit as i64)
        .fetch_all(self.db.pool())
        .await
        .map_err(|e| {
            error!(
                "查询 Commits 失败: channel_id={}, last_pts={}, error={}",
                channel_id, last_pts, e
            );
            crate::error::ServerError::Database(format!("Failed to query commits: {}", e))
        })?;

        let commits: Vec<ServerCommit> = rows
            .into_iter()
            .map(|row| ServerCommit {
                pts: row.pts as u64,
                server_msg_id: row.server_msg_id as u64,
                local_message_id: row.local_message_id.map(|v| v as u64),
                channel_id: row.channel_id as u64,
                channel_type: row.channel_type as u8,
                message_type: row.message_type,
                content: row.content,
                server_timestamp: row.server_timestamp,
                sender_id: row.sender_id as u64,
                sender_info: row.sender_username.map(|username| SenderInfo {
                    user_id: row.sender_id as u64,
                    username,
                    nickname: None,
                    avatar_url: None,
                }),
            })
            .collect();

        Ok(commits)
    }

    /// 批量查询 Commits
    pub async fn batch_query_commits(
        &self,
        queries: Vec<(u64, u64, u32)>, // (channel_id, last_pts, limit)
    ) -> Result<Vec<Vec<ServerCommit>>> {
        let mut results = Vec::new();

        for (channel_id, last_pts, limit) in queries {
            let commits = self.query_commits(channel_id, last_pts, limit).await?;
            results.push(commits);
        }

        Ok(results)
    }

    /// 获取频道的最新 Commit
    pub async fn get_latest_commit(&self, channel_id: u64) -> Result<Option<ServerCommit>> {
        use privchat_protocol::rpc::sync::SenderInfo;

        #[derive(sqlx::FromRow)]
        struct CommitRow {
            pts: i64,
            server_msg_id: i64,
            local_message_id: Option<i64>,
            channel_id: i64,
            channel_type: i16,
            message_type: String,
            content: serde_json::Value,
            server_timestamp: i64,
            sender_id: i64,
            sender_username: Option<String>,
        }

        let row = sqlx::query_as::<_, CommitRow>(
            r#"
            SELECT 
                pts, server_msg_id, local_message_id, channel_id, channel_type,
                message_type, content, server_timestamp, sender_id, sender_username
            FROM privchat_commit_log
            WHERE channel_id = $1
            ORDER BY pts DESC
            LIMIT 1
            "#,
        )
        .bind(channel_id as i64)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|e| {
            error!(
                "获取最新 Commit 失败: channel_id={}, error={}",
                channel_id, e
            );
            crate::error::ServerError::Database(format!("Failed to get latest commit: {}", e))
        })?;

        if let Some(row) = row {
            Ok(Some(ServerCommit {
                pts: row.pts as u64,
                server_msg_id: row.server_msg_id as u64,
                local_message_id: row.local_message_id.map(|v| v as u64),
                channel_id: row.channel_id as u64,
                channel_type: row.channel_type as u8,
                message_type: row.message_type,
                content: row.content,
                server_timestamp: row.server_timestamp,
                sender_id: row.sender_id as u64,
                sender_info: row.sender_username.map(|username| SenderInfo {
                    user_id: row.sender_id as u64,
                    username,
                    nickname: None,
                    avatar_url: None,
                }),
            }))
        } else {
            Ok(None)
        }
    }

    /// 清理旧数据（可选，定期任务）
    ///
    /// 保留最近 N 天的数据
    pub async fn cleanup_old_commits(&self, days: i64) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp_millis() - days * 24 * 3600 * 1000;

        debug!("清理旧 Commits: cutoff={}, days={}", cutoff, days);

        let result = sqlx::query!(
            "DELETE FROM privchat_commit_log WHERE created_at < $1",
            cutoff
        )
        .execute(self.db.pool())
        .await
        .map_err(|e| {
            error!("清理旧 Commits 失败: cutoff={}, error={}", cutoff, e);
            crate::error::ServerError::Database(format!("Failed to cleanup old commits: {}", e))
        })?;

        let deleted = result.rows_affected();
        debug!("✅ 清理完成: 删除 {} 条 Commits", deleted);

        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    // TODO: 添加单元测试
    #[tokio::test]
    async fn test_save_and_query_commits() {
        // 测试保存和查询 Commits
    }
}
