/// Channel pts 数据库操作
/// 
/// 负责 privchat_channel_pts 表的操作

use std::sync::Arc;
use tracing::{debug, error};
use crate::error::Result;
use crate::infra::database::Database;

/// Channel pts DAO
pub struct ChannelPtsDao {
    db: Arc<Database>,
}

impl ChannelPtsDao {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
    
    /// 原子递增 pts（使用数据库事务）⭐
    /// 
    /// 这是 pts 分配的核心方法，必须保证原子性
    /// 使用 PostgreSQL 的 ON CONFLICT DO UPDATE 和 RETURNING 子句实现原子递增
    pub async fn allocate_pts(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<u64> {
        debug!(
            "分配 pts: channel_id={}, channel_type={}",
            channel_id, channel_type
        );
        
        let now = chrono::Utc::now().timestamp_millis();
        
        // 使用 INSERT ... ON CONFLICT DO UPDATE ... RETURNING 实现原子递增
        // 如果记录不存在，插入初始值 1；如果存在，递增并返回新值
        let row = sqlx::query!(
            r#"
            INSERT INTO privchat_channel_pts 
            (channel_id, channel_type, current_pts, created_at, updated_at)
            VALUES ($1, $2, 1, $3, $3)
            ON CONFLICT (channel_id, channel_type) DO UPDATE
            SET current_pts = privchat_channel_pts.current_pts + 1,
                updated_at = $3
            RETURNING current_pts
            "#,
            channel_id as i64,
            channel_type as i16,
            now
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| {
            error!("分配 pts 失败: channel_id={}, channel_type={}, error={}", channel_id, channel_type, e);
            crate::error::ServerError::Database(format!("Failed to allocate pts: {}", e))
        })?;
        
        let new_pts = row.current_pts as u64;
        debug!("✅ 分配 pts 成功: channel_id={}, new_pts={}", channel_id, new_pts);
        
        Ok(new_pts)
    }
    
    /// 获取当前 pts（不递增）
    pub async fn get_current_pts(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<u64> {
        let row = sqlx::query!(
            "SELECT current_pts FROM privchat_channel_pts WHERE channel_id = $1 AND channel_type = $2",
            channel_id as i64,
            channel_type as i16
        )
        .fetch_optional(self.db.pool())
        .await
        .map_err(|e| {
            error!("查询 pts 失败: channel_id={}, channel_type={}, error={}", channel_id, channel_type, e);
            crate::error::ServerError::Database(format!("Failed to get current pts: {}", e))
        })?;
        
        Ok(row.map(|r| r.current_pts as u64).unwrap_or(0))
    }
    
    /// 批量获取多个频道的 pts
    pub async fn batch_get_current_pts(
        &self,
        channels: Vec<(u64, u8)>, // (channel_id, channel_type)
    ) -> Result<Vec<(u64, u8, u64)>> {
        if channels.is_empty() {
            return Ok(Vec::new());
        }
        
        // 构建查询条件：使用 ANY 子句
        let mut results = Vec::new();
        
        // 由于 PostgreSQL 的 UNNEST 对复合类型支持有限，我们使用循环查询
        // 对于大量频道，可以考虑使用临时表或数组参数
        for (channel_id, channel_type) in channels {
            let pts = self.get_current_pts(channel_id, channel_type).await?;
            results.push((channel_id, channel_type, pts));
        }
        
        Ok(results)
    }
    
    /// 设置 pts（用于初始化或恢复）
    pub async fn set_pts(
        &self,
        channel_id: u64,
        channel_type: u8,
        pts: u64,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        
        sqlx::query!(
            r#"
            INSERT INTO privchat_channel_pts 
            (channel_id, channel_type, current_pts, created_at, updated_at)
            VALUES ($1, $2, $3, $4, $4)
            ON CONFLICT (channel_id, channel_type) DO UPDATE
            SET current_pts = $3, updated_at = $4
            "#,
            channel_id as i64,
            channel_type as i16,
            pts as i64,
            now
        )
        .execute(self.db.pool())
        .await
        .map_err(|e| {
            error!("设置 pts 失败: channel_id={}, channel_type={}, pts={}, error={}", channel_id, channel_type, pts, e);
            crate::error::ServerError::Database(format!("Failed to set pts: {}", e))
        })?;
        
        Ok(())
    }
    
    /// 批量初始化 pts（从其他表恢复）
    pub async fn batch_init_pts(
        &self,
        channel_pts_list: Vec<(u64, u8, u64)>, // (channel_id, channel_type, pts)
    ) -> Result<()> {
        // TODO: 实现批量插入
        for (channel_id, channel_type, pts) in channel_pts_list {
            self.set_pts(channel_id, channel_type, pts).await?;
        }
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_allocate_pts() {
        // 测试 pts 原子递增
    }
    
    #[tokio::test]
    async fn test_concurrent_allocate_pts() {
        // 测试并发场景下的 pts 分配
        // 确保没有重复的 pts
    }
}
