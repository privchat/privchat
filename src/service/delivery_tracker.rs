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

use redis::{AsyncCommands, Client as RedisClient, Script};
use tracing::{debug, warn};

type UserId = u64;
type ChannelId = u64;

/// 送达水位追踪器
///
/// 维护每用户每频道的"连续送达 pts"（delivered_contiguous_pts）。
/// 核心不变量：该水位只在 pts == current + 1 时才推进，保证连续性。
///
/// Redis 数据结构：HASH  key = `dpts:{user_id}`  field = `{channel_id}`  value = pts
#[derive(Clone)]
pub struct DeliveryTracker {
    redis_client: RedisClient,
}

impl DeliveryTracker {
    pub fn new(redis_url: &str) -> Result<Self, crate::error::ServerError> {
        let redis_client = RedisClient::open(redis_url)
            .map_err(|e| crate::error::ServerError::Internal(format!("Redis 连接失败: {}", e)))?;
        Ok(Self { redis_client })
    }

    fn hash_key(user_id: UserId) -> String {
        format!("dpts:{}", user_id)
    }

    /// 尝试推进连续送达水位（原子操作）
    ///
    /// 仅当 pts == current + 1 时才推进，返回 true。
    /// 否则返回 false（有间隔，不推进）。
    pub async fn try_advance(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
        pts: u64,
    ) -> bool {
        let key = Self::hash_key(user_id);
        let field = channel_id.to_string();

        // Lua 脚本保证原子性：只有 pts == current + 1 时才 HSET
        let script = Script::new(
            r#"
            local current = tonumber(redis.call('HGET', KEYS[1], ARGV[1]) or '0')
            local new_pts = tonumber(ARGV[2])
            if new_pts == current + 1 then
                redis.call('HSET', KEYS[1], ARGV[1], new_pts)
                return 1
            elseif new_pts <= current then
                return 2
            else
                return 0
            end
            "#,
        );

        let mut conn = match self.redis_client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!("DeliveryTracker: Redis 连接失败: {}", e);
                return false;
            }
        };

        match script
            .key(&key)
            .arg(&field)
            .arg(pts)
            .invoke_async::<i64>(&mut conn)
            .await
        {
            Ok(1) => {
                debug!(
                    "📦 DeliveryTracker: 推进 user={} channel={} pts={}",
                    user_id, channel_id, pts
                );
                true
            }
            Ok(2) => {
                // pts <= current，已经送达过，忽略
                true
            }
            Ok(0) => {
                debug!(
                    "📦 DeliveryTracker: 存在间隔 user={} channel={} pts={}",
                    user_id, channel_id, pts
                );
                false
            }
            Ok(_) => false,
            Err(e) => {
                warn!("DeliveryTracker: try_advance 失败: {}", e);
                false
            }
        }
    }

    /// 批量连续推进（用于离线消息批量投递 / sync 拉取）
    ///
    /// 给定有序的 pts 列表 [p1, p2, ..., pN]，从当前水位开始尽可能连续推进。
    /// 返回最终水位。
    pub async fn batch_advance(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
        sorted_pts_list: &[u64],
    ) -> u64 {
        if sorted_pts_list.is_empty() {
            return self.get_delivered_pts(user_id, channel_id).await;
        }

        let key = Self::hash_key(user_id);
        let field = channel_id.to_string();

        // Lua：从 current 开始，连续推进 sorted list 中连续的 pts
        let script = Script::new(
            r#"
            local current = tonumber(redis.call('HGET', KEYS[1], ARGV[1]) or '0')
            local count = tonumber(ARGV[2])
            for i = 1, count do
                local p = tonumber(ARGV[2 + i])
                if p == current + 1 then
                    current = p
                elseif p > current + 1 then
                    break
                end
            end
            redis.call('HSET', KEYS[1], ARGV[1], current)
            return current
            "#,
        );

        let mut conn = match self.redis_client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!("DeliveryTracker: Redis 连接失败: {}", e);
                return 0;
            }
        };

        let mut prepared = script.prepare_invoke();
        prepared.key(&key);
        prepared.arg(&field);
        prepared.arg(sorted_pts_list.len());
        for &p in sorted_pts_list {
            prepared.arg(p);
        }

        match prepared.invoke_async::<u64>(&mut conn).await {
            Ok(final_pts) => {
                debug!(
                    "📦 DeliveryTracker: batch_advance user={} channel={} final_pts={}",
                    user_id, channel_id, final_pts
                );
                final_pts
            }
            Err(e) => {
                warn!("DeliveryTracker: batch_advance 失败: {}", e);
                0
            }
        }
    }

    /// 查询当前连续送达水位
    pub async fn get_delivered_pts(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
    ) -> u64 {
        let key = Self::hash_key(user_id);
        let field = channel_id.to_string();

        let mut conn = match self.redis_client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!("DeliveryTracker: Redis 连接失败: {}", e);
                return 0;
            }
        };

        match conn.hget::<_, _, Option<u64>>(&key, &field).await {
            Ok(Some(pts)) => pts,
            Ok(None) => 0,
            Err(e) => {
                warn!("DeliveryTracker: get_delivered_pts 失败: {}", e);
                0
            }
        }
    }

    /// 强制设置水位（用于数据修复 / 初始化迁移）
    pub async fn force_set(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
        pts: u64,
    ) {
        let key = Self::hash_key(user_id);
        let field = channel_id.to_string();

        let mut conn = match self.redis_client.get_multiplexed_async_connection().await {
            Ok(c) => c,
            Err(e) => {
                warn!("DeliveryTracker: Redis 连接失败: {}", e);
                return;
            }
        };

        if let Err(e) = conn.hset::<_, _, u64, ()>(&key, &field, pts).await {
            warn!("DeliveryTracker: force_set 失败: {}", e);
        }
    }
}
