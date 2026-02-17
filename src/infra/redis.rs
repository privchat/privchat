// RedisClient - Redis客户端实现
// 基于 bb8-redis 连接池

use bb8::Pool;
use bb8_redis::RedisConnectionManager;
use redis::AsyncCommands;
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

    /// KEYS pattern - 查找匹配的 key（用于查找用户的所有设备 presence）
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
