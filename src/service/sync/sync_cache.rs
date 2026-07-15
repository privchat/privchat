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
use crate::infra::redis::RedisClient;
use privchat_protocol::rpc::sync::ServerCommit;
/// Redis 缓存操作
///
/// 负责同步相关的 Redis 缓存
use std::sync::Arc;
use tracing::{debug, error, warn};

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
    /// Key: commit_log:{channel_id}
    /// Type: Sorted Set
    /// Score: pts
    /// Value: JSON(commit)
    pub async fn cache_commit(&self, commit: &ServerCommit) -> Result<()> {
        let key = format!("commit_log:{}", commit.channel_id);

        debug!(
            "缓存 Commit: key={}, pts={}, server_msg_id={}",
            key, commit.pts, commit.server_msg_id
        );

        // 序列化 Commit
        let value = serde_json::to_string(commit).map_err(|e| {
            error!("序列化 Commit 失败: {}", e);
            crate::error::ServerError::Internal(format!("Failed to serialize commit: {}", e))
        })?;

        // 添加到 Sorted Set
        self.redis
            .zadd(&key, commit.pts as f64, &value)
            .await
            .map_err(|e| {
                error!("Redis ZADD 失败: key={}, error={}", key, e);
                e
            })?;

        // 只保留最近 100 条（删除排名 0 到 -101 的元素，保留最后 100 条）
        self.redis
            .zremrangebyrank(&key, 0, -101)
            .await
            .map_err(|e| {
                warn!("Redis ZREMRANGEBYRANK 失败: key={}, error={}", key, e);
                e
            })?;

        // 设置过期时间（1 小时）
        self.redis.expire(&key, 3600).await.map_err(|e| {
            warn!("Redis EXPIRE 失败: key={}, error={}", key, e);
            e
        })?;

        Ok(())
    }

    /// 从 Redis 查询 Commits
    ///
    /// ZRANGEBYSCORE commit_log:{channel_id} {last_pts+1} +inf LIMIT 0 {limit}
    pub async fn query_commits_from_cache(
        &self,
        channel_id: u64,
        last_pts: u64,
        limit: u32,
    ) -> Result<Option<Vec<ServerCommit>>> {
        let key = format!("commit_log:{}", channel_id);

        debug!(
            "从缓存查询 Commits: key={}, last_pts={}, limit={}",
            key, last_pts, limit
        );

        let values: Vec<String> = self
            .redis
            .zrangebyscore(&key, (last_pts + 1) as f64, f64::MAX, Some(limit as usize))
            .await
            .map_err(|e| {
                debug!("Redis ZRANGEBYSCORE 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!(
                    "Failed to query commits from cache: {}",
                    e
                ))
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
    /// Key: pts:{channel_id}
    /// Type: String
    /// Value: pts (u64)
    /// TTL: 10 秒
    pub async fn cache_pts(&self, channel_id: u64, pts: u64) -> Result<()> {
        let key = format!("pts:{}", channel_id);

        debug!("缓存 pts: key={}, pts={}", key, pts);

        self.redis
            .setex(&key, 10, &pts.to_string())
            .await
            .map_err(|e| {
                warn!("Redis SETEX 失败: key={}, error={}", key, e);
                crate::error::ServerError::Internal(format!("Failed to cache pts: {}", e))
            })?;

        Ok(())
    }

    /// 从 Redis 获取 pts
    pub async fn get_pts_from_cache(&self, channel_id: u64) -> Result<Option<u64>> {
        let key = format!("pts:{}", channel_id);

        match self.redis.get(&key).await {
            Ok(Some(value)) => match value.parse::<u64>() {
                Ok(pts) => {
                    debug!("✅ pts 缓存命中: key={}, pts={}", key, pts);
                    Ok(Some(pts))
                }
                Err(e) => {
                    warn!("解析 pts 失败: key={}, value={}, error={}", key, value, e);
                    Ok(None)
                }
            },
            Ok(None) => Ok(None),
            Err(e) => {
                debug!("Redis GET 失败: key={}, error={}", key, e);
                Ok(None) // 缓存失败不影响主流程
            }
        }
    }

    /// 批量缓存 pts
    pub async fn batch_cache_pts(
        &self,
        channel_pts_list: Vec<(u64, u64)>, // (channel_id, pts)
    ) -> Result<()> {
        for (channel_id, pts) in channel_pts_list {
            self.cache_pts(channel_id, pts).await?;
        }
        Ok(())
    }

    // ============================================================
    // 清理
    // ============================================================

    /// 清理频道的所有缓存
    pub async fn clear_channel_cache(&self, channel_id: u64) -> Result<()> {
        let commit_key = format!("commit_log:{}", channel_id);
        let pts_key = format!("pts:{}", channel_id);
        debug!("清理频道缓存: channel_id={}", channel_id);

        // 删除所有相关 key（忽略单个删除失败，继续删除其他）
        if let Err(e) = self.redis.del(&commit_key).await {
            warn!("删除 commit 缓存失败: key={}, error={}", commit_key, e);
        }

        if let Err(e) = self.redis.del(&pts_key).await {
            warn!("删除 pts 缓存失败: key={}, error={}", pts_key, e);
        }

        debug!("✅ 频道缓存清理完成: channel_id={}", channel_id);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn test_cache_and_query_commits() {
        // 测试 Commit 缓存和查询
    }

    #[tokio::test]
    async fn test_pts_cache() {
        // 测试 pts 缓存
    }

}
