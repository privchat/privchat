/// Redis 缓存操作
/// 
/// 负责同步相关的 Redis 缓存

use std::sync::Arc;
use tracing::{debug, error, warn};
use privchat_protocol::rpc::sync::ServerCommit;
use crate::error::Result;
use crate::infra::redis::RedisClient;

/// 同步缓存
pub struct SyncCache {
    redis: Arc<RedisClient>,
}

impl SyncCache {
    pub fn new(redis: Arc<RedisClient>) -> Self {
        Self { redis }
    }
    
    // ============================================================
    // Commit 缓存
    // ============================================================
    
    /// 缓存 Commit 到 Redis
    /// 
    /// Key: commit_log:{channel_id}:{channel_type}
    /// Type: Sorted Set
    /// Score: pts
    /// Value: JSON(commit)
    pub async fn cache_commit(&self, commit: &ServerCommit) -> Result<()> {
        let key = format!("commit_log:{}:{}", commit.channel_id, commit.channel_type);
        
        debug!(
            "缓存 Commit: key={}, pts={}, server_msg_id={}",
            key, commit.pts, commit.server_msg_id
        );
        
        // 序列化 Commit
        let value = serde_json::to_string(commit)
            .map_err(|e| {
                error!("序列化 Commit 失败: {}", e);
                crate::error::ServerError::Internal(format!("Failed to serialize commit: {}", e))
            })?;
        
        // 添加到 Sorted Set
        self.redis.zadd(&key, commit.pts as f64, &value).await
            .map_err(|e| {
                error!("Redis ZADD 失败: key={}, error={}", key, e);
                e
            })?;
        
        // 只保留最近 100 条（删除排名 0 到 -101 的元素，保留最后 100 条）
        self.redis.zremrangebyrank(&key, 0, -101).await
            .map_err(|e| {
                warn!("Redis ZREMRANGEBYRANK 失败: key={}, error={}", key, e);
                e
            })?;
        
        // 设置过期时间（1 小时）
        self.redis.expire(&key, 3600).await
            .map_err(|e| {
                warn!("Redis EXPIRE 失败: key={}, error={}", key, e);
                e
            })?;
        
        Ok(())
    }
    
    /// 从 Redis 查询 Commits
    /// 
    /// ZRANGEBYSCORE commit_log:{channel_id}:{channel_type} {last_pts+1} +inf LIMIT 0 {limit}
    pub async fn query_commits_from_cache(
        &self,
        channel_id: u64,
        channel_type: u8,
        last_pts: u64,
        limit: u32,
    ) -> Result<Option<Vec<ServerCommit>>> {
        let key = format!("commit_log:{}:{}", channel_id, channel_type);
        
        debug!(
            "从缓存查询 Commits: key={}, last_pts={}, limit={}",
            key, last_pts, limit
        );
        
        let values: Vec<String> = self.redis
            .zrangebyscore(&key, (last_pts + 1) as f64, f64::MAX, Some(limit as usize))
            .await
            .map_err(|e| {
                debug!("Redis ZRANGEBYSCORE 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!("Failed to query commits from cache: {}", e))
            })?;
        
        if values.is_empty() {
            // Cache Miss
            return Ok(None);
        }
        
        // 反序列化
        let mut commits = Vec::new();
        for value in values {
            match serde_json::from_str::<ServerCommit>(&value) {
                Ok(commit) => commits.push(commit),
                Err(e) => {
                    warn!("反序列化 Commit 失败: value={}, error={}", value, e);
                    // 继续处理其他 commits
                }
            }
        }
        
        if commits.is_empty() {
            return Ok(None);
        }
        
        debug!("✅ 缓存命中: 返回 {} 条 commits", commits.len());
        
        Ok(Some(commits))
    }
    
    /// 批量缓存 Commits
    pub async fn batch_cache_commits(&self, commits: &[ServerCommit]) -> Result<()> {
        for commit in commits {
            self.cache_commit(commit).await?;
        }
        Ok(())
    }
    
    // ============================================================
    // pts 缓存
    // ============================================================
    
    /// 缓存频道 pts
    /// 
    /// Key: pts:{channel_id}:{channel_type}
    /// Type: String
    /// Value: pts (u64)
    /// TTL: 10 秒
    pub async fn cache_pts(
        &self,
        channel_id: u64,
        channel_type: u8,
        pts: u64,
    ) -> Result<()> {
        let key = format!("pts:{}:{}", channel_id, channel_type);
        
        debug!("缓存 pts: key={}, pts={}", key, pts);
        
        self.redis.setex(&key, 10, &pts.to_string()).await
            .map_err(|e| {
                warn!("Redis SETEX 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!("Failed to cache pts: {}", e))
            })?;
        
        Ok(())
    }
    
    /// 从 Redis 获取 pts
    pub async fn get_pts_from_cache(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<Option<u64>> {
        let key = format!("pts:{}:{}", channel_id, channel_type);
        
        match self.redis.get(&key).await {
            Ok(Some(value)) => {
                match value.parse::<u64>() {
                    Ok(pts) => {
                        debug!("✅ pts 缓存命中: key={}, pts={}", key, pts);
                        Ok(Some(pts))
                    }
                    Err(e) => {
                        warn!("解析 pts 失败: key={}, value={}, error={}", key, value, e);
                        Ok(None)
                    }
                }
            }
            Ok(None) => Ok(None),
            Err(e) => {
                debug!("Redis GET 失败: key={}, error={}", key, e);
                Ok(None)  // 缓存失败不影响主流程
            }
        }
    }
    
    /// 批量缓存 pts
    pub async fn batch_cache_pts(
        &self,
        channel_pts_list: Vec<(u64, u8, u64)>, // (channel_id, channel_type, pts)
    ) -> Result<()> {
        for (channel_id, channel_type, pts) in channel_pts_list {
            self.cache_pts(channel_id, channel_type, pts).await?;
        }
        Ok(())
    }
    
    // ============================================================
    // 在线用户 Fan-out
    // ============================================================
    
    /// 推送给在线用户
    /// 
    /// Pub/Sub: user_msgs:{user_id}
    pub async fn fanout_to_online_users(
        &self,
        user_ids: &[u64],
        commit: &ServerCommit,
    ) -> Result<()> {
        debug!(
            "Fan-out 推送: user_count={}, channel_id={}, pts={}",
            user_ids.len(), commit.channel_id, commit.pts
        );
        
        let message = serde_json::to_string(commit)
            .map_err(|e| {
                error!("序列化 Commit 失败: {}", e);
                crate::error::ServerError::Internal(format!("Failed to serialize commit: {}", e))
            })?;
        
        for user_id in user_ids {
            let channel = format!("user_msgs:{}", user_id);
            if let Err(e) = self.redis.publish(&channel, &message).await {
                warn!("Fan-out 推送失败: user_id={}, channel={}, error={}", user_id, channel, e);
                // 继续推送其他用户，不中断
            }
        }
        
        debug!("✅ Fan-out 完成: 推送给 {} 个用户", user_ids.len());
        
        Ok(())
    }
    
    // ============================================================
    // 在线用户管理
    // ============================================================
    
    /// 获取频道的在线用户列表
    /// 
    /// Key: online_users:{channel_id}:{channel_type}
    /// Type: Set
    pub async fn get_online_users(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<Vec<u64>> {
        let key = format!("online_users:{}:{}", channel_id, channel_type);
        
        let members = self.redis.smembers(&key).await
            .map_err(|e| {
                debug!("Redis SMEMBERS 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!("Failed to get online users: {}", e))
            })?;
        
        let user_ids: Vec<u64> = members
            .into_iter()
            .filter_map(|s: String| s.parse().ok())
            .collect();
        
        Ok(user_ids)
    }
    
    /// 添加在线用户
    pub async fn add_online_user(
        &self,
        channel_id: u64,
        channel_type: u8,
        user_id: u64,
    ) -> Result<()> {
        let key = format!("online_users:{}:{}", channel_id, channel_type);
        
        self.redis.sadd(&key, &user_id.to_string()).await
            .map_err(|e| {
                warn!("Redis SADD 失败: key={}, user_id={}, error={}", key, user_id, e);
                crate::error::ServerError::Internal(format!("Failed to add online user: {}", e))
            })?;
        
        self.redis.expire(&key, 3600).await
            .map_err(|e| {
                warn!("Redis EXPIRE 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!("Failed to set expire: {}", e))
            })?; // 1 小时过期
        
        Ok(())
    }
    
    /// 移除在线用户
    pub async fn remove_online_user(
        &self,
        channel_id: u64,
        channel_type: u8,
        _user_id: u64,
    ) -> Result<()> {
        let _key = format!("online_users:{}:{}", channel_id, channel_type);
        
        // TODO: 实现 Redis 操作
        /*
        self.redis.srem(&key, user_id.to_string()).await?;
        */
        
        Ok(())
    }
    
    // ============================================================
    // 清理
    // ============================================================
    
    /// 清理频道的所有缓存
    pub async fn clear_channel_cache(
        &self,
        channel_id: u64,
        channel_type: u8,
    ) -> Result<()> {
        let commit_key = format!("commit_log:{}:{}", channel_id, channel_type);
        let pts_key = format!("pts:{}:{}", channel_id, channel_type);
        let online_key = format!("online_users:{}:{}", channel_id, channel_type);
        
        debug!("清理频道缓存: channel_id={}, channel_type={}", channel_id, channel_type);
        
        // 删除所有相关 key（忽略单个删除失败，继续删除其他）
        if let Err(e) = self.redis.del(&commit_key).await {
            warn!("删除 commit 缓存失败: key={}, error={}", commit_key, e);
        }
        
        if let Err(e) = self.redis.del(&pts_key).await {
            warn!("删除 pts 缓存失败: key={}, error={}", pts_key, e);
        }
        
        if let Err(e) = self.redis.del(&online_key).await {
            warn!("删除在线用户缓存失败: key={}, error={}", online_key, e);
        }
        
        debug!("✅ 频道缓存清理完成: channel_id={}", channel_id);
        
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_cache_and_query_commits() {
        // 测试 Commit 缓存和查询
    }
    
    #[tokio::test]
    async fn test_pts_cache() {
        // 测试 pts 缓存
    }
    
    #[tokio::test]
    async fn test_fanout() {
        // 测试 Fan-out 推送
    }
}
