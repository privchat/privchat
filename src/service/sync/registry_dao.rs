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
use privchat_protocol::rpc::sync::{ClientSubmitResponse, ServerDecision};
/// 客户端消息号注册表操作
///
/// 负责 privchat_client_msg_registry 表的操作（幂等性保证）
use std::sync::Arc;
use tracing::{debug, error};

/// 客户端消息号注册表 DAO
pub struct ClientMsgRegistryDao {
    db: Arc<Database>,
}

impl ClientMsgRegistryDao {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    /// 检查是否重复提交
    ///
    /// CODEX-8：幂等命名空间 = (sender_id, device_id, local_message_id)。sender/device 来自认证
    /// 会话（服务端权威）。此前按 local_message_id 单列全局判重 —— 客户端雪花 worker 位空间小，
    /// 跨用户/跨设备碰撞会把后到的消息静默判重成前者（丢消息）。
    pub async fn check_duplicate(
        &self,
        sender_id: u64,
        device_id: &str,
        local_message_id: u64,
    ) -> Result<Option<ClientSubmitResponse>> {
        debug!(
            "检查重复提交: sender_id={}, device_id={}, local_message_id={}",
            sender_id, device_id, local_message_id
        );

        #[derive(sqlx::FromRow)]
        struct RegistryRow {
            server_msg_id: i64,
            pts: i64,
            channel_id: i64,
            channel_type: i16,
            decision: String,
            created_at: i64,
        }

        let row = sqlx::query_as::<_, RegistryRow>(
            r#"
            SELECT
                server_msg_id, pts, channel_id, channel_type,
                decision, created_at
            FROM privchat_client_msg_registry
            WHERE sender_id = $1 AND device_id = $2 AND local_message_id = $3
            "#,
        )
        .bind(sender_id as i64)
        .bind(device_id)
        .bind(local_message_id as i64)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|e| {
            error!(
                "检查重复提交失败: sender_id={}, local_message_id={}, error={}",
                sender_id, local_message_id, e
            );
            crate::error::ServerError::Database(format!("Failed to check duplicate: {}", e))
        })?;

        if let Some(row) = row {
            // 重复提交，返回之前的响应
            let decision = match row.decision.as_str() {
                "accepted" => ServerDecision::Accepted,
                "transformed" => ServerDecision::Transformed {
                    reason: "Server transformed the message".to_string(),
                },
                "rejected" => ServerDecision::Rejected {
                    reason: "Message was rejected".to_string(),
                },
                _ => ServerDecision::Accepted,
            };

            return Ok(Some(ClientSubmitResponse {
                decision,
                pts: Some(row.pts as u64),
                server_msg_id: Some(row.server_msg_id as u64),
                server_timestamp: row.created_at,
                local_message_id,
                has_gap: false,
                current_pts: row.pts as u64,
            }));
        }

        Ok(None)
    }

    /// 注册 local_message_id（幂等占位，命名空间 = sender+device+lmid，见 [check_duplicate]）。
    pub async fn register(
        &self,
        local_message_id: u64,
        server_msg_id: u64,
        pts: u64,
        channel_id: u64,
        channel_type: u8,
        sender_id: u64,
        device_id: &str,
        decision: &str, // "accepted" | "transformed" | "rejected"
    ) -> Result<()> {
        debug!(
            "注册 local_message_id: sender_id={}, device_id={}, local_message_id={}, server_msg_id={}, pts={}",
            sender_id, device_id, local_message_id, server_msg_id, pts
        );

        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query(
            r#"
            INSERT INTO privchat_client_msg_registry
            (local_message_id, server_msg_id, pts, channel_id, channel_type, sender_id, device_id, decision, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            ON CONFLICT (sender_id, device_id, local_message_id) DO NOTHING
            "#,
        )
        .bind(local_message_id as i64)
        .bind(server_msg_id as i64)
        .bind(pts as i64)
        .bind(channel_id as i64)
        .bind(channel_type as i16)
        .bind(sender_id as i64)
        .bind(device_id)
        .bind(decision)
        .bind(now)
        .execute(self.db.pool())
        .await
        .map_err(|e| {
            error!("注册 local_message_id 失败: local_message_id={}, error={}", local_message_id, e);
            crate::error::ServerError::Database(format!("Failed to register local_message_id: {}", e))
        })?;

        Ok(())
    }

    /// 清理旧数据（定期任务）
    ///
    /// 清理 N 天前的记录
    pub async fn cleanup_old_records(&self, days: i64) -> Result<u64> {
        let cutoff = chrono::Utc::now().timestamp_millis() - days * 24 * 3600 * 1000;

        debug!("清理旧注册记录: cutoff={}", cutoff);

        let result = sqlx::query!(
            "DELETE FROM privchat_client_msg_registry WHERE created_at < $1",
            cutoff
        )
        .execute(self.db.pool())
        .await
        .map_err(|e| {
            error!("清理旧注册记录失败: cutoff={}, error={}", cutoff, e);
            crate::error::ServerError::Database(format!("Failed to cleanup old records: {}", e))
        })?;

        let deleted = result.rows_affected();
        debug!("✅ 清理完成: 删除 {} 条记录", deleted);

        Ok(deleted)
    }

    /// 批量检查是否重复（同 [check_duplicate] 的 sender+device 命名空间）
    pub async fn batch_check_duplicate(
        &self,
        sender_id: u64,
        device_id: &str,
        local_message_ids: Vec<u64>,
    ) -> Result<Vec<(u64, bool)>> {
        if local_message_ids.is_empty() {
            return Ok(Vec::new());
        }

        let ids: Vec<i64> = local_message_ids.iter().map(|id| *id as i64).collect();

        #[derive(sqlx::FromRow)]
        struct RegistryRow {
            local_message_id: i64,
        }

        let rows = sqlx::query_as::<_, RegistryRow>(
            "SELECT local_message_id FROM privchat_client_msg_registry \
             WHERE sender_id = $1 AND device_id = $2 AND local_message_id = ANY($3)",
        )
        .bind(sender_id as i64)
        .bind(device_id)
        .bind(&ids)
        .fetch_all(self.db.pool())
        .await
        .map_err(|e| {
            error!("批量检查重复失败: error={}", e);
            crate::error::ServerError::Database(format!("Failed to batch check duplicate: {}", e))
        })?;

        let existing: std::collections::HashSet<u64> = rows
            .into_iter()
            .map(|r| r.local_message_id as u64)
            .collect();

        let results: Vec<(u64, bool)> = local_message_ids
            .into_iter()
            .map(|id| (id, existing.contains(&id)))
            .collect();

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_register_and_check_duplicate() {
        // 测试注册和幂等性检查
    }

    #[tokio::test]
    async fn test_cleanup_old_records() {
        // 测试清理旧记录
    }
}
