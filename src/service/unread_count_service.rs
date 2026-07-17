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

use crate::error::ServerError;
/// 未读计数服务（适配器）
///
/// 功能：
/// - 增加未读计数
/// - 获取所有未读计数
/// - 清空所有未读计数
/// - 清空特定会话的未读计数
///
/// 数据结构：
/// - 使用现有的 CacheManager.unread_counts
/// - L1: Moka 本地缓存（低延迟）
/// - L2: Redis 缓存（持久化 + 多节点共享）
use crate::infra::cache::{CacheManager, TwoLevelCache, UnreadCounts};
use crate::infra::redis::RedisClient;
use chrono::Utc;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::info;
use uuid::Uuid;

const UNREAD_LOCK_STRIPES: usize = 256;
const UNREAD_MEMBER_UPDATE_CONCURRENCY: usize = 64;

/// 未读计数服务（适配器）
#[derive(Clone)]
pub struct UnreadCountService {
    cache_manager: Arc<CacheManager>,
    redis_client: Option<Arc<RedisClient>>,
    ttl_seconds: u64, // 默认 7 天
    lock_stripes: Arc<Vec<Mutex<()>>>,
}

impl UnreadCountService {
    /// 创建新的未读计数服务
    pub fn new(cache_manager: Arc<CacheManager>) -> Self {
        Self::new_inner(cache_manager, None)
    }

    /// 创建使用 Redis 原子计数的未读计数服务。
    pub fn new_with_redis(
        cache_manager: Arc<CacheManager>,
        redis_client: Arc<RedisClient>,
    ) -> Self {
        Self::new_inner(cache_manager, Some(redis_client))
    }

    fn new_inner(cache_manager: Arc<CacheManager>, redis_client: Option<Arc<RedisClient>>) -> Self {
        Self {
            cache_manager,
            redis_client,
            ttl_seconds: 7 * 24 * 3600,
            lock_stripes: Arc::new((0..UNREAD_LOCK_STRIPES).map(|_| Mutex::new(())).collect()),
        }
    }

    /// 设置 TTL
    pub fn with_ttl(mut self, seconds: u64) -> Self {
        self.ttl_seconds = seconds;
        self
    }

    fn lock_for_user(&self, user_id: u64) -> &Mutex<()> {
        &self.lock_stripes[(user_id as usize) % self.lock_stripes.len()]
    }

    fn add_count(current: i32, delta: u64) -> i32 {
        let delta = i32::try_from(delta).unwrap_or(i32::MAX);
        current.saturating_add(delta)
    }

    fn normalize_count(count: u64) -> i32 {
        i32::try_from(count).unwrap_or(i32::MAX)
    }

    fn normalize_count_i64(count: u64) -> i64 {
        i64::try_from(count).unwrap_or(i64::MAX)
    }

    fn redis_key(user_id: u64) -> String {
        format!("unread_counts:{}", user_id)
    }

    fn redis_field(channel_id: u64) -> String {
        channel_id.to_string()
    }

    async fn refresh_redis_ttl(
        redis: &RedisClient,
        key: &str,
        ttl_seconds: u64,
    ) -> Result<(), ServerError> {
        if ttl_seconds > 0 {
            redis.expire(key, ttl_seconds as usize).await?;
        }
        Ok(())
    }

    /// 增加未读计数
    ///
    /// 参数:
    /// - user_id: 用户 ID（u64）
    /// - channel_id: 频道 ID（u64）
    /// - count: 增加的数量（默认为 1）
    pub async fn increment(
        &self,
        user_id: u64,
        channel_id: u64,
        count: u64,
    ) -> Result<(), ServerError> {
        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let field = Self::redis_field(channel_id);
            redis
                .hincrby(&key, &field, Self::normalize_count_i64(count))
                .await?;
            Self::refresh_redis_ttl(redis, &key, self.ttl_seconds).await?;
            info!(
                "📊 增加未读计数(Redis): user={}, channel={}, +{}",
                user_id, channel_id, count
            );
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        // 1. 获取或创建 UnreadCounts
        let mut counts = self
            .cache_manager
            .unread_counts
            .get(&user_id)
            .await
            .unwrap_or_else(|| UnreadCounts {
                user_id,
                counts: HashMap::new(),
                updated_at: Utc::now(),
            });

        // 2. 增加计数
        let current = counts.counts.entry(channel_id).or_insert(0);
        *current = Self::add_count(*current, count);
        counts.updated_at = Utc::now();

        // 3. 写回缓存（L1+L2 同时写入）⭐
        self.cache_manager
            .unread_counts
            .put(user_id, counts, self.ttl_seconds)
            .await;

        info!(
            "📊 增加未读计数: user={}, channel={}, +{}",
            user_id, channel_id, count
        );

        Ok(())
    }

    /// 批量增加未读计数
    pub async fn increment_batch(
        &self,
        user_id: u64,
        channel_counts: HashMap<u64, u64>,
    ) -> Result<(), ServerError> {
        if channel_counts.is_empty() {
            return Ok(());
        }

        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let increments: Vec<_> = channel_counts
                .into_iter()
                .map(|(channel_id, count)| {
                    (
                        Self::redis_field(channel_id),
                        Self::normalize_count_i64(count),
                    )
                })
                .collect();
            redis
                .hincrby_many_expire(&key, &increments, self.ttl_seconds as usize)
                .await?;
            info!("📊 批量增加未读计数(Redis): user={}", user_id);
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        // 获取现有计数
        let mut counts = self
            .cache_manager
            .unread_counts
            .get(&user_id)
            .await
            .unwrap_or_else(|| UnreadCounts {
                user_id,
                counts: HashMap::new(),
                updated_at: Utc::now(),
            });

        // 批量增加
        for (channel_id, count) in channel_counts {
            let current = counts.counts.entry(channel_id).or_insert(0);
            *current = Self::add_count(*current, count);
        }

        counts.updated_at = Utc::now();

        // 写回缓存
        self.cache_manager
            .unread_counts
            .put(user_id, counts, self.ttl_seconds)
            .await;

        info!("📊 批量增加未读计数: user={}", user_id);

        Ok(())
    }

    /// 为一个频道的多个成员批量增加未读计数（并发执行）
    pub async fn increment_for_channel_members(
        &self,
        channel_id: u64,
        user_ids: &[u64],
        count: u64,
    ) -> Result<(), ServerError> {
        if user_ids.is_empty() {
            return Ok(());
        }

        if let Some(redis) = &self.redis_client {
            let field = Self::redis_field(channel_id);
            let delta = Self::normalize_count_i64(count);
            let increments: Vec<_> = user_ids
                .iter()
                .map(|user_id| (Self::redis_key(*user_id), field.clone(), delta))
                .collect();
            redis
                .hincrby_multi_keys_expire(&increments, self.ttl_seconds as usize)
                .await?;
            return Ok(());
        }

        stream::iter(user_ids.iter().copied())
            .for_each_concurrent(UNREAD_MEMBER_UPDATE_CONCURRENCY, |user_id| async move {
                if let Err(e) = self.increment(user_id, channel_id, count).await {
                    tracing::warn!("⚠️ 批量增加未读计数失败: {}", e);
                }
            })
            .await;

        Ok(())
    }

    /// 获取所有未读计数
    ///
    /// 返回: HashMap<channel_id, unread_count>
    pub async fn get_all(&self, user_id: u64) -> Result<HashMap<u64, u64>, ServerError> {
        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let raw = redis.hgetall_i64(&key).await?;
            let result = raw
                .into_iter()
                .filter_map(|(field, count)| {
                    if count <= 0 {
                        return None;
                    }
                    field
                        .parse::<u64>()
                        .ok()
                        .map(|channel_id| (channel_id, count as u64))
                })
                .collect();
            return Ok(result);
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        if let Some(counts) = self.cache_manager.unread_counts.get(&user_id).await {
            // 转换 u64 -> u64
            let result = counts
                .counts
                .iter()
                .filter(|(_, &count)| count > 0) // 只返回 > 0 的
                .map(|(k, v)| (*k, *v as u64))
                .collect();

            info!(
                "📊 获取未读计数: user={}, channels={}",
                user_id,
                counts.counts.len()
            );

            Ok(result)
        } else {
            Ok(HashMap::new())
        }
    }

    /// 获取特定会话的未读计数
    pub async fn get(&self, user_id: u64, channel_id: u64) -> Result<u64, ServerError> {
        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let field = Self::redis_field(channel_id);
            return Ok(redis.hget_i64(&key, &field).await?.unwrap_or(0).max(0) as u64);
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        if let Some(counts) = self.cache_manager.unread_counts.get(&user_id).await {
            let count = counts.counts.get(&channel_id).copied().unwrap_or(0);
            Ok(count.max(0) as u64)
        } else {
            Ok(0)
        }
    }

    /// 清空所有未读计数
    pub async fn clear_all(&self, user_id: u64) -> Result<(), ServerError> {
        if let Some(redis) = &self.redis_client {
            redis.del(&Self::redis_key(user_id)).await?;
            info!("🗑️ 清空所有未读计数(Redis): user={}", user_id);
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        // 从 L1 和 L2 删除
        self.cache_manager.unread_counts.invalidate(&user_id).await;

        info!("🗑️ 清空所有未读计数: user={}", user_id);

        Ok(())
    }

    /// 清空特定会话的未读计数
    pub async fn clear_channel(&self, user_id: u64, channel_id: u64) -> Result<(), ServerError> {
        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let fields = vec![Self::redis_field(channel_id)];
            redis.hdel_fields(&key, &fields).await?;
            info!(
                "🗑️ 清空会话未读计数(Redis): user={}, channel={}",
                user_id, channel_id
            );
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        if let Some(mut counts) = self.cache_manager.unread_counts.get(&user_id).await {
            counts.counts.remove(&channel_id);
            counts.updated_at = Utc::now();

            // 如果所有会话都清空了，删除整个缓存
            if counts.counts.is_empty() {
                self.cache_manager.unread_counts.invalidate(&user_id).await;
            } else {
                // 否则更新缓存
                self.cache_manager
                    .unread_counts
                    .put(user_id, counts, self.ttl_seconds)
                    .await;
            }

            info!(
                "🗑️ 清空会话未读计数: user={}, channel={}",
                user_id, channel_id
            );
        }

        Ok(())
    }

    /// 批量清空会话未读计数
    pub async fn clear_channels(
        &self,
        user_id: u64,
        channel_ids: &[u64],
    ) -> Result<(), ServerError> {
        if channel_ids.is_empty() {
            return Ok(());
        }

        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let fields: Vec<_> = channel_ids.iter().copied().map(Self::redis_field).collect();
            redis.hdel_fields(&key, &fields).await?;
            info!(
                "🗑️ 批量清空会话未读计数(Redis): user={}, channels={}",
                user_id,
                channel_ids.len()
            );
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        if let Some(mut counts) = self.cache_manager.unread_counts.get(&user_id).await {
            for channel_id in channel_ids {
                counts.counts.remove(channel_id);
            }

            counts.updated_at = Utc::now();

            if counts.counts.is_empty() {
                self.cache_manager.unread_counts.invalidate(&user_id).await;
            } else {
                self.cache_manager
                    .unread_counts
                    .put(user_id, counts, self.ttl_seconds)
                    .await;
            }

            info!(
                "🗑️ 批量清空会话未读计数: user={}, channels={}",
                user_id,
                channel_ids.len()
            );
        }

        Ok(())
    }

    /// 设置未读计数（直接设置，不增加）
    pub async fn set(&self, user_id: u64, channel_id: u64, count: u64) -> Result<(), ServerError> {
        if let Some(redis) = &self.redis_client {
            let key = Self::redis_key(user_id);
            let field = Self::redis_field(channel_id);
            redis
                .hset_i64(&key, &field, Self::normalize_count_i64(count))
                .await?;
            Self::refresh_redis_ttl(redis, &key, self.ttl_seconds).await?;
            info!(
                "📊 设置未读计数(Redis): user={}, channel={}, count={}",
                user_id, channel_id, count
            );
            return Ok(());
        }

        let _guard = self.lock_for_user(user_id).lock().await;

        let mut counts = self
            .cache_manager
            .unread_counts
            .get(&user_id)
            .await
            .unwrap_or_else(|| UnreadCounts {
                user_id,
                counts: HashMap::new(),
                updated_at: Utc::now(),
            });

        counts
            .counts
            .insert(channel_id, Self::normalize_count(count));
        counts.updated_at = Utc::now();

        self.cache_manager
            .unread_counts
            .put(user_id, counts, self.ttl_seconds)
            .await;

        info!(
            "📊 设置未读计数: user={}, channel={}, count={}",
            user_id, channel_id, count
        );

        Ok(())
    }

    /// 解析 UUID（辅助方法）
    /// 解析或生成 UUID
    ///
    /// - 如果输入是有效的 UUID 格式，直接解析
    /// - 如果输入是普通字符串（如 "alice"），基于字符串生成确定性 UUID
    fn parse_uuid(id: &str) -> Result<Uuid, ServerError> {
        // 尝试直接解析 UUID
        if let Ok(uuid) = Uuid::parse_str(id) {
            return Ok(uuid);
        }

        // 如果不是有效的 UUID，基于字符串生成确定性 UUID
        // 使用简单的哈希方法：将字符串的字节重复填充到 16 字节
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        let hash = hasher.finish();

        // 使用哈希值生成 UUID（重复填充到 16 字节）
        let mut bytes = [0u8; 16];
        bytes[0..8].copy_from_slice(&hash.to_le_bytes());
        bytes[8..16].copy_from_slice(&hash.to_be_bytes());

        Ok(Uuid::from_bytes(bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::cache::CacheManager;

    #[tokio::test]
    async fn test_increment_and_get() {
        let cache_manager = Arc::new(CacheManager::new());
        let service = UnreadCountService::new(cache_manager);

        let user_id = 1001_u64;
        let channel_id = 3001_u64;

        // 增加计数
        service
            .increment(user_id, channel_id, 1)
            .await
            .expect("增加失败");
        service
            .increment(user_id, channel_id, 2)
            .await
            .expect("增加失败");

        // 获取计数
        let count = service.get(user_id, channel_id).await.expect("获取失败");
        assert_eq!(count, 3);

        // 获取所有
        let all_counts = service.get_all(user_id).await.expect("获取失败");
        assert_eq!(all_counts.len(), 1);
        assert_eq!(all_counts.get(&channel_id), Some(&3));
    }

    #[tokio::test]
    async fn test_concurrent_increment_keeps_all_updates() {
        let cache_manager = Arc::new(CacheManager::new());
        let service = Arc::new(UnreadCountService::new(cache_manager));

        let user_id = 1002_u64;
        let channel_id = 3003_u64;
        let tasks: Vec<_> = (0..200)
            .map(|_| {
                let service = service.clone();
                tokio::spawn(async move {
                    service
                        .increment(user_id, channel_id, 1)
                        .await
                        .expect("增加失败");
                })
            })
            .collect();

        for task in tasks {
            task.await.expect("task join failed");
        }

        let count = service.get(user_id, channel_id).await.expect("获取失败");
        assert_eq!(count, 200);
    }

    #[tokio::test]
    async fn test_clear_channel() {
        let cache_manager = Arc::new(CacheManager::new());
        let service = UnreadCountService::new(cache_manager);

        let user_id = 1001_u64;
        let channel1 = 3001_u64;
        let channel2 = 3002_u64;

        // 增加计数
        service
            .increment(user_id, channel1, 5)
            .await
            .expect("增加失败");
        service
            .increment(user_id, channel2, 10)
            .await
            .expect("增加失败");

        // 清空一个会话
        service
            .clear_channel(user_id, channel1)
            .await
            .expect("清空失败");

        // 验证
        let count1 = service.get(user_id, channel1).await.expect("获取失败");
        let count2 = service.get(user_id, channel2).await.expect("获取失败");
        assert_eq!(count1, 0);
        assert_eq!(count2, 10);
    }

    #[tokio::test]
    async fn test_clear_all() {
        let cache_manager = Arc::new(CacheManager::new());
        let service = UnreadCountService::new(cache_manager);

        let user_id = 1001_u64;
        let channel1 = 3001_u64;
        let channel2 = 3002_u64;

        // 增加计数
        service
            .increment(user_id, channel1, 5)
            .await
            .expect("增加失败");
        service
            .increment(user_id, channel2, 10)
            .await
            .expect("增加失败");

        // 清空所有
        service.clear_all(user_id).await.expect("清空失败");

        // 验证
        let all_counts = service.get_all(user_id).await.expect("获取失败");
        assert_eq!(all_counts.len(), 0);
    }
}
