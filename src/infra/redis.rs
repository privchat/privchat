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

// RedisClient - Redis客户端实现
// 基于 bb8-redis 连接池

use bb8::Pool;
use bb8_redis::RedisConnectionManager;
use redis::AsyncCommands;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::config::RedisConfig;

/// Redis 客户端（基于连接池）
pub struct RedisClient {
    pool: Arc<Pool<RedisConnectionManager>>,
    /// 单条 Redis 命令的执行超时
    command_timeout: Duration,
}

impl RedisClient {
    /// 创建新的 Redis 客户端（从 RedisConfig 配置）
    pub async fn new(config: &RedisConfig) -> Result<Self, crate::error::ServerError> {
        let manager = RedisConnectionManager::new(config.url.clone()).map_err(|e| {
            crate::error::ServerError::Internal(format!("Failed to create Redis manager: {}", e))
        })?;

        let pool = Pool::builder()
            .max_size(config.pool_size)
            .min_idle(Some(config.min_idle))
            .connection_timeout(config.connection_timeout())
            .idle_timeout(Some(config.idle_timeout()))
            .build(manager)
            .await
            .map_err(|e| {
                crate::error::ServerError::Internal(format!("Failed to create Redis pool: {}", e))
            })?;

        let command_timeout = config.command_timeout();

        // 测试连接
        {
            let mut conn = pool.get().await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Failed to get Redis connection: {}",
                    e
                ))
            })?;

            let _: String = conn.ping().await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis ping failed: {}", e))
            })?;
        }

        tracing::info!(
            "✅ Redis 连接池已创建 (pool_size={}, min_idle={}, conn_timeout={}s, cmd_timeout={}ms, idle_timeout={}s)",
            config.pool_size,
            config.min_idle,
            config.connection_timeout_secs,
            config.command_timeout_ms,
            config.idle_timeout_secs,
        );

        Ok(Self {
            pool: Arc::new(pool),
            command_timeout,
        })
    }

    /// 获取连接池状态（活跃连接数、空闲连接数）
    pub fn pool_state(&self) -> bb8::State {
        self.pool.state()
    }

    /// 从连接池获取连接
    async fn get_conn(
        &self,
    ) -> Result<bb8::PooledConnection<'_, RedisConnectionManager>, crate::error::ServerError> {
        self.pool.get().await.map_err(|e| {
            crate::error::ServerError::Internal(format!("Failed to get Redis connection: {}", e))
        })
    }

    /// 执行带超时的 Redis 操作
    async fn with_timeout<F, T>(&self, op: F) -> Result<T, crate::error::ServerError>
    where
        F: std::future::Future<Output = Result<T, crate::error::ServerError>>,
    {
        tokio::time::timeout(self.command_timeout, op)
            .await
            .map_err(|_| {
                crate::error::ServerError::Internal(format!(
                    "Redis command timeout ({}ms)",
                    self.command_timeout.as_millis()
                ))
            })?
    }

    // ============================================================
    // String 操作
    // ============================================================

    /// SET key value
    pub async fn set(&self, key: &str, value: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.set::<_, _, ()>(key, value).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis SET failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// SETEX key seconds value
    pub async fn setex(
        &self,
        key: &str,
        seconds: usize,
        value: &str,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.set_ex::<_, _, ()>(key, value, seconds as u64)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis SETEX failed: {}", e))
                })?;
            Ok(())
        })
        .await
    }

    /// SET key value NX EX seconds. Returns false when another owner exists.
    pub async fn set_nx_ex(
        &self,
        key: &str,
        seconds: usize,
        value: &str,
    ) -> Result<bool, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Option<String> = redis::cmd("SET")
                .arg(key)
                .arg(value)
                .arg("NX")
                .arg("EX")
                .arg(seconds)
                .query_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis SET NX EX failed: {e}"))
                })?;
            Ok(result.as_deref() == Some("OK"))
        })
        .await
    }

    /// Refresh a lease only when the fencing token still matches.
    pub async fn compare_and_expire(
        &self,
        key: &str,
        expected_value: &str,
        seconds: usize,
    ) -> Result<bool, crate::error::ServerError> {
        const SCRIPT: &str = r#"
            if redis.call('GET', KEYS[1]) == ARGV[1] then
                return redis.call('EXPIRE', KEYS[1], ARGV[2])
            end
            return 0
        "#;
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let refreshed: i64 = redis::Script::new(SCRIPT)
                .key(key)
                .arg(expected_value)
                .arg(seconds)
                .invoke_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!(
                        "Redis compare-and-expire failed: {e}"
                    ))
                })?;
            Ok(refreshed == 1)
        })
        .await
    }

    /// GET key
    pub async fn get(&self, key: &str) -> Result<Option<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Option<String> = conn.get(key).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis GET failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    /// GETDEL key —— 原子取值并删除（一次性 token 的跨实例消费语义，Redis 6.2+/KeyDB）。
    pub async fn getdel(&self, key: &str) -> Result<Option<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Option<String> = redis::cmd("GETDEL")
                .arg(key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis GETDEL failed: {}", e))
                })?;
            Ok(result)
        })
        .await
    }

    /// DEL key
    pub async fn del(&self, key: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.del::<_, ()>(key).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis DEL failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// EXPIRE key seconds
    pub async fn expire(&self, key: &str, seconds: usize) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.expire::<_, ()>(key, seconds as i64)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis EXPIRE failed: {}", e))
                })?;
            Ok(())
        })
        .await
    }

    /// EXISTS key - 检查 key 是否存在
    pub async fn exists(&self, key: &str) -> Result<bool, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: bool = conn.exists(key).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis EXISTS failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    // ============================================================
    // Hash 操作
    // ============================================================

    /// HINCRBY key field delta
    pub async fn hincrby(
        &self,
        key: &str,
        field: &str,
        delta: i64,
    ) -> Result<i64, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: i64 = redis::cmd("HINCRBY")
                .arg(key)
                .arg(field)
                .arg(delta)
                .query_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis HINCRBY failed: {}", e))
                })?;
            Ok(result)
        })
        .await
    }

    /// 批量 HINCRBY，并可选刷新 key TTL。
    pub async fn hincrby_many_expire(
        &self,
        key: &str,
        increments: &[(String, i64)],
        expire_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        if increments.is_empty() {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let mut pipe = redis::pipe();
            for (field, delta) in increments {
                pipe.cmd("HINCRBY").arg(key).arg(field).arg(*delta).ignore();
            }
            if expire_seconds > 0 {
                pipe.cmd("EXPIRE").arg(key).arg(expire_seconds).ignore();
            }
            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis HINCRBY pipeline failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// Batch HINCRBY across multiple keys in one Redis pipeline. This is used
    /// for group unread fanout where one message updates many users.
    pub async fn hincrby_multi_keys_expire(
        &self,
        increments: &[(String, String, i64)],
        expire_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        if increments.is_empty() {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let mut pipe = redis::pipe();
            for (key, field, delta) in increments {
                pipe.cmd("HINCRBY").arg(key).arg(field).arg(*delta).ignore();
                if expire_seconds > 0 {
                    pipe.cmd("EXPIRE").arg(key).arg(expire_seconds).ignore();
                }
            }
            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Redis multi-key HINCRBY pipeline failed: {e}"
                ))
            })?;
            Ok(())
        })
        .await
    }

    /// HGET key field
    pub async fn hget_i64(
        &self,
        key: &str,
        field: &str,
    ) -> Result<Option<i64>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Option<i64> = redis::cmd("HGET")
                .arg(key)
                .arg(field)
                .query_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis HGET failed: {}", e))
                })?;
            Ok(result)
        })
        .await
    }

    /// HGETALL key
    pub async fn hgetall_i64(
        &self,
        key: &str,
    ) -> Result<HashMap<String, i64>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: HashMap<String, i64> = redis::cmd("HGETALL")
                .arg(key)
                .query_async(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis HGETALL failed: {}", e))
                })?;
            Ok(result)
        })
        .await
    }

    /// HSET key field value
    pub async fn hset_i64(
        &self,
        key: &str,
        field: &str,
        value: i64,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            redis::cmd("HSET")
                .arg(key)
                .arg(field)
                .arg(value)
                .query_async::<usize>(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis HSET failed: {}", e))
                })?;
            Ok(())
        })
        .await
    }

    /// HDEL key field [field ...]
    pub async fn hdel_fields(
        &self,
        key: &str,
        fields: &[String],
    ) -> Result<(), crate::error::ServerError> {
        if fields.is_empty() {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let mut cmd = redis::cmd("HDEL");
            cmd.arg(key);
            for field in fields {
                cmd.arg(field);
            }
            cmd.query_async::<usize>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis HDEL failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// KEYS pattern - 查找匹配的 key。
    ///
    /// 注意：不要在业务热路径使用；在线态判断应走 ConnectionManager 或 O(1) presence 索引。
    pub async fn keys(&self, pattern: &str) -> Result<Vec<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Vec<String> = conn.keys(pattern).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis KEYS failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    // ============================================================
    // Sorted Set 操作
    // ============================================================

    /// ZADD key score member
    pub async fn zadd(
        &self,
        key: &str,
        score: f64,
        member: &str,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.zadd::<_, _, _, ()>(key, member, score)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis ZADD failed: {}", e))
                })?;
            Ok(())
        })
        .await
    }

    /// ZRANGEBYSCORE key min max [LIMIT offset count]
    pub async fn zrangebyscore(
        &self,
        key: &str,
        min: f64,
        max: f64,
        limit: Option<usize>,
    ) -> Result<Vec<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let mut cmd = redis::cmd("ZRANGEBYSCORE");
            cmd.arg(key).arg(min).arg(max);
            if let Some(limit) = limit {
                cmd.arg("LIMIT").arg(0).arg(limit);
            }
            let result: Vec<String> = cmd.query_async(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis ZRANGEBYSCORE failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    /// ZREMRANGEBYRANK key start stop
    pub async fn zremrangebyrank(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.zremrangebyrank::<_, ()>(key, start, stop)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!(
                        "Redis ZREMRANGEBYRANK failed: {}",
                        e
                    ))
                })?;
            Ok(())
        })
        .await
    }

    pub async fn zremrangebyscore(
        &self,
        key: &str,
        min: f64,
        max: f64,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            redis::cmd("ZREMRANGEBYSCORE")
                .arg(key)
                .arg(min)
                .arg(max)
                .query_async::<usize>(&mut *conn)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!(
                        "Redis ZREMRANGEBYSCORE failed: {e}"
                    ))
                })?;
            Ok(())
        })
        .await
    }

    pub async fn zrem(&self, key: &str, member: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.zrem::<_, _, ()>(key, member).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis ZREM failed: {e}"))
            })?;
            Ok(())
        })
        .await
    }

    // ============================================================
    // Set 操作
    // ============================================================

    /// SADD key member
    pub async fn sadd(&self, key: &str, member: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.sadd::<_, _, ()>(key, member).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis SADD failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// SREM key member
    pub async fn srem(&self, key: &str, member: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.srem::<_, _, ()>(key, member).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis SREM failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// SMEMBERS key
    pub async fn smembers(&self, key: &str) -> Result<Vec<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Vec<String> = conn.smembers(key).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis SMEMBERS failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    // ============================================================
    // List 操作
    // ============================================================

    /// LPUSH key value
    pub async fn lpush(&self, key: &str, value: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.lpush::<_, _, ()>(key, value).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis LPUSH failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    pub async fn rpush(&self, key: &str, value: &str) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.rpush::<_, _, ()>(key, value).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis RPUSH failed: {e}"))
            })?;
            Ok(())
        })
        .await
    }

    /// Non-blocking LPOP. Cross-node dispatch deliberately uses polling on the
    /// shared short-command pool; blocking commands require a dedicated Redis
    /// connection and must not outlive `command_timeout`.
    pub async fn lpop(&self, key: &str) -> Result<Option<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.lpop::<_, Option<String>>(key, None)
                .await
                .map_err(|e| crate::error::ServerError::Internal(format!("Redis LPOP failed: {e}")))
        })
        .await
    }

    /// LTRIM key start stop
    pub async fn ltrim(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.ltrim::<_, ()>(key, start, stop).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis LTRIM failed: {}", e))
            })?;
            Ok(())
        })
        .await
    }

    /// LPUSH + LTRIM + EXPIRE pipeline，用于固定长度 recent buffer。
    pub async fn lpush_ltrim_expire(
        &self,
        key: &str,
        value: &str,
        limit: usize,
        ttl_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        if limit == 0 {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let stop = limit.saturating_sub(1) as isize;
            let mut pipe = redis::pipe();
            pipe.lpush(key, value).ignore();
            pipe.ltrim(key, 0, stop).ignore();
            if ttl_seconds > 0 {
                pipe.expire(key, ttl_seconds as i64).ignore();
            }

            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Redis recent buffer pipeline failed: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
    }

    /// 多条 LPUSH + LTRIM + EXPIRE pipeline，用于单用户离线队列批量写入。
    pub async fn lpush_many_ltrim_expire(
        &self,
        key: &str,
        values: &[String],
        limit: usize,
        ttl_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        if values.is_empty() || limit == 0 {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let stop = limit.saturating_sub(1) as isize;
            let mut pipe = redis::pipe();
            for value in values {
                pipe.lpush(key, value).ignore();
            }
            pipe.ltrim(key, 0, stop).ignore();
            if ttl_seconds > 0 {
                pipe.expire(key, ttl_seconds as i64).ignore();
            }

            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Redis list batch pipeline failed: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
    }

    /// 对多个 key 写入同一条消息，并为每个 key 执行 LTRIM + EXPIRE。
    pub async fn lpush_to_many_ltrim_expire(
        &self,
        keys: &[String],
        value: &str,
        limit: usize,
        ttl_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        if keys.is_empty() || limit == 0 {
            return Ok(());
        }

        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let stop = limit.saturating_sub(1) as isize;
            let mut pipe = redis::pipe();
            for key in keys {
                pipe.lpush(key, value).ignore();
                pipe.ltrim(key, 0, stop).ignore();
                if ttl_seconds > 0 {
                    pipe.expire(key, ttl_seconds as i64).ignore();
                }
            }

            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Redis multi-key list pipeline failed: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
    }

    /// 用给定列表替换 key，并重新应用 LTRIM + EXPIRE。
    ///
    /// values 约定为 Redis LRANGE 的顺序（最新 -> 最旧）。
    pub async fn replace_list_ltrim_expire(
        &self,
        key: &str,
        values: &[String],
        limit: usize,
        ttl_seconds: usize,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let mut pipe = redis::pipe();
            pipe.del(key).ignore();

            if !values.is_empty() && limit > 0 {
                for value in values.iter().rev() {
                    pipe.lpush(key, value).ignore();
                }
                pipe.ltrim(key, 0, limit.saturating_sub(1) as isize)
                    .ignore();
                if ttl_seconds > 0 {
                    pipe.expire(key, ttl_seconds as i64).ignore();
                }
            }

            pipe.query_async::<()>(&mut *conn).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Redis list replace pipeline failed: {}",
                    e
                ))
            })?;
            Ok(())
        })
        .await
    }

    /// LRANGE key start stop
    pub async fn lrange(
        &self,
        key: &str,
        start: isize,
        stop: isize,
    ) -> Result<Vec<String>, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: Vec<String> = conn.lrange(key, start, stop).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis LRANGE failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    /// LLEN key
    pub async fn llen(&self, key: &str) -> Result<usize, crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            let result: usize = conn.llen(key).await.map_err(|e| {
                crate::error::ServerError::Internal(format!("Redis LLEN failed: {}", e))
            })?;
            Ok(result)
        })
        .await
    }

    // ============================================================
    // Pub/Sub 操作
    // ============================================================

    /// PUBLISH channel message
    pub async fn publish(
        &self,
        channel: &str,
        message: &str,
    ) -> Result<(), crate::error::ServerError> {
        self.with_timeout(async {
            let mut conn = self.get_conn().await?;
            conn.publish::<_, _, ()>(channel, message)
                .await
                .map_err(|e| {
                    crate::error::ServerError::Internal(format!("Redis PUBLISH failed: {}", e))
                })?;
            Ok(())
        })
        .await
    }
}
