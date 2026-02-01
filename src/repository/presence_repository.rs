use sqlx::{PgPool, Row, postgres::PgRow};
use tracing::{debug, info};

use crate::error::ServerError;

/// 在线状态数据仓库
pub struct PresenceRepository {
    pool: PgPool,
}

impl PresenceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    
    /// 获取用户的最后上线时间
    /// 
    /// # 参数
    /// - user_id: 用户ID
    /// 
    /// # 返回
    /// - Some(timestamp): 最后上线时间（Unix 时间戳）
    /// - None: 用户从未上线或数据已过期
    pub async fn get_last_seen(&self, user_id: u64) -> Result<Option<i64>, ServerError> {
        let result = sqlx::query(
            "SELECT last_seen_at FROM privchat_user_last_seen WHERE user_id = $1"
        )
        .bind(user_id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Internal(format!("Failed to get last_seen: {}", e)))?;
        
        Ok(result.map(|row: PgRow| row.get::<i64, _>("last_seen_at")))
    }
    
    /// 批量获取用户的最后上线时间
    /// 
    /// # 参数
    /// - user_ids: 用户ID列表
    /// 
    /// # 返回
    /// - HashMap<user_id, timestamp>
    pub async fn batch_get_last_seen(
        &self,
        user_ids: &[u64],
    ) -> Result<std::collections::HashMap<u64, i64>, ServerError> {
        if user_ids.is_empty() {
            return Ok(std::collections::HashMap::new());
        }
        
        // 构造 IN 查询（PostgreSQL）
        let placeholders = (1..=user_ids.len()).map(|i| format!("${}", i)).collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT user_id, last_seen_at FROM privchat_user_last_seen WHERE user_id IN ({})",
            placeholders
        );
        
        let mut q = sqlx::query(&query);
        for user_id in user_ids {
            q = q.bind(*user_id as i64);
        }
        
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| ServerError::Internal(format!("Failed to batch get last_seen: {}", e)))?;
        
        let mut results = std::collections::HashMap::new();
        for row in rows {
            let user_id: i64 = row.get("user_id");
            let user_id = user_id as u64;
            let last_seen_at = row.get::<i64, _>("last_seen_at");
            results.insert(user_id, last_seen_at);
        }
        
        Ok(results)
    }
    
    /// 更新单个用户的最后上线时间
    /// 
    /// # 参数
    /// - user_id: 用户ID
    /// - last_seen_at: 最后上线时间（Unix 时间戳）
    pub async fn update_last_seen(
        &self,
        user_id: u64,
        last_seen_at: i64,
    ) -> Result<(), ServerError> {
        sqlx::query(
            "INSERT INTO privchat_user_last_seen (user_id, last_seen_at) 
             VALUES ($1, $2) 
             ON CONFLICT (user_id) DO UPDATE SET last_seen_at = $2"
        )
        .bind(user_id as i64)
        .bind(last_seen_at)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Internal(format!("Failed to update last_seen: {}", e)))?;
        
        debug!("Updated last_seen for user {}: {}", user_id, last_seen_at);
        Ok(())
    }
    
    /// 批量更新用户的最后上线时间
    /// 
    /// # 参数
    /// - updates: Vec<(user_id, last_seen_at)>
    /// 
    /// # 性能
    /// - 使用批量 INSERT ... ON DUPLICATE KEY UPDATE
    /// - 每次最多更新 1000 条
    pub async fn batch_update_last_seen(
        &self,
        updates: &[(u64, i64)],
    ) -> Result<(), ServerError> {
        if updates.is_empty() {
            return Ok(());
        }
        
        // 分批更新，每批 1000 条
        const BATCH_SIZE: usize = 1000;
        
        for chunk in updates.chunks(BATCH_SIZE) {
            // 构造批量插入语句（PostgreSQL）
            let mut param_idx = 1;
            let placeholders = chunk.iter()
                .map(|_| {
                    let p = format!("(${}, ${})", param_idx, param_idx + 1);
                    param_idx += 2;
                    p
                })
                .collect::<Vec<_>>()
                .join(",");
            
            let query = format!(
                "INSERT INTO privchat_user_last_seen (user_id, last_seen_at) VALUES {} 
                 ON CONFLICT (user_id) DO UPDATE SET last_seen_at = EXCLUDED.last_seen_at",
                placeholders
            );
            
            let mut q = sqlx::query(&query);
            for (user_id, last_seen_at) in chunk {
                q = q.bind(*user_id as i64).bind(*last_seen_at);
            }
            
            q.execute(&self.pool)
                .await
                .map_err(|e| ServerError::Internal(format!("Failed to batch update last_seen: {}", e)))?;
            
            debug!("Batch updated {} last_seen records", chunk.len());
        }
        
        info!("✅ Batch updated {} user last_seen records", updates.len());
        Ok(())
    }
    
    /// 清理过期数据
    /// 
    /// # 参数
    /// - days: 保留最近多少天的数据
    /// - limit: 每次删除的最大行数（避免锁表）
    /// 
    /// # 返回
    /// - 删除的行数
    pub async fn cleanup_expired(
        &self,
        days: u32,
        limit: u32,
    ) -> Result<u64, ServerError> {
        let threshold = chrono::Utc::now().timestamp() - (days as i64 * 86400);
        
        let result = sqlx::query(
            "DELETE FROM privchat_user_last_seen 
             WHERE user_id IN (
                 SELECT user_id FROM privchat_user_last_seen 
                 WHERE last_seen_at < $1 
                 LIMIT $2
             )"
        )
        .bind(threshold)
        .bind(limit as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Internal(format!("Failed to cleanup expired data: {}", e)))?;
        
        let deleted = result.rows_affected();
        if deleted > 0 {
            info!("🧹 Cleaned up {} expired last_seen records (older than {} days)", deleted, days);
        }
        
        Ok(deleted)
    }
    
    /// 获取统计信息
    pub async fn get_stats(&self) -> Result<PresenceStats, ServerError> {
        let row = sqlx::query(
            "SELECT 
                COUNT(*) as total_records,
                COUNT(DISTINCT user_id) as unique_users,
                MIN(last_seen_at) as oldest_record,
                MAX(last_seen_at) as newest_record
             FROM privchat_user_last_seen"
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::Internal(format!("Failed to get stats: {}", e)))?;
        
        Ok(PresenceStats {
            total_records: row.get::<i64, _>("total_records") as u64,
            unique_users: row.get::<i64, _>("unique_users") as u64,
            oldest_record: row.try_get::<i64, _>("oldest_record").ok(),
            newest_record: row.try_get::<i64, _>("newest_record").ok(),
        })
    }
}

/// 在线状态统计
#[derive(Debug)]
pub struct PresenceStats {
    pub total_records: u64,
    pub unique_users: u64,
    pub oldest_record: Option<i64>,
    pub newest_record: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    // 注意：这些测试需要数据库连接
    // 在实际运行前需要配置测试数据库
    
    #[tokio::test]
    #[ignore] // 需要数据库
    async fn test_update_and_get_last_seen() {
        // let pool = create_test_pool().await;
        // let repo = PresenceRepository::new(pool);
        // 
        // let user_id = 12345;
        // let timestamp = 1234567890;
        // 
        // repo.update_last_seen(user_id, timestamp).await.unwrap();
        // 
        // let result = repo.get_last_seen(user_id).await.unwrap();
        // assert_eq!(result, Some(timestamp));
    }
}
