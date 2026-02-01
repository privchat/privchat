/// 客户端消息号注册表操作
/// 
/// 负责 privchat_client_msg_registry 表的操作（幂等性保证）

use std::sync::Arc;
use tracing::{debug, error};
use privchat_protocol::rpc::sync::{ClientSubmitResponse, ServerDecision};
use crate::error::Result;
use crate::infra::database::Database;

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
    /// SELECT * FROM privchat_client_msg_registry WHERE local_message_id = ?
    pub async fn check_duplicate(
        &self,
        local_message_id: u64,
    ) -> Result<Option<ClientSubmitResponse>> {
        debug!("检查重复提交: local_message_id={}", local_message_id);
        
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
            WHERE local_message_id = $1
            "#
        )
        .bind(local_message_id as i64)
        .fetch_optional(self.db.pool())
        .await
        .map_err(|e| {
            error!("检查重复提交失败: local_message_id={}, error={}", local_message_id, e);
            crate::error::ServerError::Database(format!("Failed to check duplicate: {}", e))
        })?;
        
        if let Some(row) = row {
            // 重复提交，返回之前的响应
            let decision = match row.decision.as_str() {
                "accepted" => ServerDecision::Accepted,
                "transformed" => ServerDecision::Transformed { reason: "Server transformed the message".to_string() },
                "rejected" => ServerDecision::Rejected { reason: "Message was rejected".to_string() },
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
    
    /// 注册 local_message_id
    /// 
    /// INSERT INTO privchat_client_msg_registry (...)
    pub async fn register(
        &self,
        local_message_id: u64,
        server_msg_id: u64,
        pts: u64,
        channel_id: u64,
        channel_type: u8,
        sender_id: u64,
        decision: &str, // "accepted" | "transformed" | "rejected"
    ) -> Result<()> {
        debug!(
            "注册 local_message_id: local_message_id={}, server_msg_id={}, pts={}",
            local_message_id, server_msg_id, pts
        );
        
        let now = chrono::Utc::now().timestamp_millis();
        
        sqlx::query!(
            r#"
            INSERT INTO privchat_client_msg_registry 
            (local_message_id, server_msg_id, pts, channel_id, channel_type, sender_id, decision, created_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (local_message_id) DO NOTHING
            "#,
            local_message_id as i64,
            server_msg_id as i64,
            pts as i64,
            channel_id as i64,
            channel_type as i16,
            sender_id as i64,
            decision,
            now
        )
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
    
    /// 批量检查是否重复
    pub async fn batch_check_duplicate(
        &self,
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
            "SELECT local_message_id FROM privchat_client_msg_registry WHERE local_message_id = ANY($1)"
        )
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
    use super::*;
    
    #[tokio::test]
    async fn test_register_and_check_duplicate() {
        // 测试注册和幂等性检查
    }
    
    #[tokio::test]
    async fn test_cleanup_old_records() {
        // 测试清理旧记录
    }
}
