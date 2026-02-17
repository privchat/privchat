/// 离线消息队列服务（使用 Redis List）
///
/// 功能：
/// - 推送离线消息（自动限制 5000 条）
/// - 批量获取离线消息
/// - 获取队列长度
/// - 清空队列
///
/// 数据结构：
/// - Redis List: offline:{user_id}:messages
/// - 上限: 5000 条（LTRIM 自动清理最旧的）
/// - 过期: 7 天
use crate::error::ServerError;
use privchat_protocol::protocol::PushMessageRequest;
use redis::{AsyncCommands, Client as RedisClient};
use tracing::info;

/// 离线消息队列服务
#[derive(Clone)]
pub struct OfflineQueueService {
    redis_client: RedisClient,
    max_queue_size: usize, // 默认 5000
    expire_seconds: i64,   // 默认 7 天
}

impl OfflineQueueService {
    /// 创建新的离线队列服务
    pub fn new(redis_url: &str) -> Result<Self, ServerError> {
        let redis_client = RedisClient::open(redis_url)
            .map_err(|e| ServerError::Internal(format!("Redis 连接失败: {}", e)))?;

        Ok(Self {
            redis_client,
            max_queue_size: 100,       // ⭐ 100 条（不是 5000）
            expire_seconds: 24 * 3600, // ⭐ 24 小时（不是 7 天）
        })
    }

    /// 设置队列大小上限
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_queue_size = max_size;
        self
    }

    /// 设置过期时间（秒）
    pub fn with_expire_seconds(mut self, seconds: i64) -> Self {
        self.expire_seconds = seconds;
        self
    }

    /// 添加离线消息（自动限制 100 条，Pipeline 单次 RTT）
    pub async fn add(&self, user_id: u64, message: &PushMessageRequest) -> Result<(), ServerError> {
        let key = Self::queue_key(user_id);
        let value = serde_json::to_string(message)
            .map_err(|e| ServerError::Internal(format!("序列化消息失败: {}", e)))?;

        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        let max_index = (self.max_queue_size - 1) as isize;

        // Pipeline: LPUSH + LTRIM + EXPIRE 合并为单次 RTT
        redis::pipe()
            .lpush(&key, &value)
            .ignore()
            .ltrim(&key, 0, max_index)
            .ignore()
            .expire(&key, self.expire_seconds)
            .ignore()
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| ServerError::Internal(format!("Pipeline 执行失败: {}", e)))?;

        info!(
            "📥 推送离线消息: user={}, 队列上限={}",
            user_id, self.max_queue_size
        );

        Ok(())
    }

    /// 批量推送离线消息（Pipeline 单次 RTT）
    pub async fn push_batch(
        &self,
        user_id: u64,
        messages: &[PushMessageRequest],
    ) -> Result<(), ServerError> {
        if messages.is_empty() {
            return Ok(());
        }

        let key = Self::queue_key(user_id);

        let values: Result<Vec<String>, _> = messages
            .iter()
            .map(|msg| serde_json::to_string(msg))
            .collect();

        let values = values.map_err(|e| ServerError::Internal(format!("序列化消息失败: {}", e)))?;

        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        let max_index = (self.max_queue_size - 1) as isize;

        // Pipeline: 所有 LPUSH + LTRIM + EXPIRE 合并为单次 RTT
        let mut pipe = redis::pipe();
        for value in &values {
            pipe.lpush(&key, value).ignore();
        }
        pipe.ltrim(&key, 0, max_index).ignore();
        pipe.expire(&key, self.expire_seconds).ignore();

        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| ServerError::Internal(format!("Pipeline 执行失败: {}", e)))?;

        info!(
            "📥 批量推送离线消息(pipeline): user={}, count={}, 队列上限={}",
            user_id,
            messages.len(),
            self.max_queue_size
        );

        Ok(())
    }

    /// 批量为多个用户添加同一条离线消息（Redis Pipeline，单次网络往返）
    pub async fn add_batch_users(
        &self,
        user_ids: &[u64],
        message: &PushMessageRequest,
    ) -> Result<(), ServerError> {
        if user_ids.is_empty() {
            return Ok(());
        }

        let value = serde_json::to_string(message)
            .map_err(|e| ServerError::Internal(format!("序列化消息失败: {}", e)))?;

        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        let max_index = (self.max_queue_size - 1) as isize;

        let mut pipe = redis::pipe();
        for &user_id in user_ids {
            let key = Self::queue_key(user_id);
            pipe.lpush(key.clone(), value.clone()).ignore();
            pipe.ltrim(key.clone(), 0, max_index).ignore();
            pipe.expire(key, self.expire_seconds).ignore();
        }

        pipe.query_async::<()>(&mut conn)
            .await
            .map_err(|e| ServerError::Internal(format!("Pipeline 执行失败: {}", e)))?;

        info!(
            "📥 Pipeline 批量推送离线消息: users={}, 队列上限={}",
            user_ids.len(),
            self.max_queue_size
        );

        Ok(())
    }

    /// 批量获取离线消息（指定范围）
    ///
    /// 参数:
    /// - start: 起始索引（0 表示最新）
    /// - end: 结束索引（-1 表示所有）
    ///
    /// 示例:
    /// - get_batch(user_id, 0, 49) - 获取最新 50 条
    /// - get_batch(user_id, 0, -1) - 获取所有
    pub async fn get_batch(
        &self,
        user_id: u64,
        start: isize,
        end: isize,
    ) -> Result<Vec<PushMessageRequest>, ServerError> {
        let key = Self::queue_key(user_id);
        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        let values: Vec<String> = conn
            .lrange(&key, start, end)
            .await
            .map_err(|e| ServerError::Internal(format!("LRANGE 失败: {}", e)))?;

        let messages: Vec<PushMessageRequest> = values
            .iter()
            .filter_map(|v| serde_json::from_str(v).ok())
            .collect();

        info!(
            "📤 获取离线消息: user={}, range={}..{}, count={}",
            user_id,
            start,
            end,
            messages.len()
        );

        Ok(messages)
    }

    /// 获取所有离线消息
    pub async fn get_all(&self, user_id: u64) -> Result<Vec<PushMessageRequest>, ServerError> {
        self.get_batch(user_id, 0, -1).await
    }

    /// 基于 pts 范围获取离线消息
    ///
    /// 参数:
    /// - min_pts: 最小 pts（不包含），只返回 pts > min_pts 的消息
    ///
    /// 注意：由于 PushMessageRequest 中没有 pts 字段，此方法需要配合 UserMessageIndex 使用
    /// 实际实现中，应该使用 UserMessageIndex 查找 pts > min_pts 的消息ID，
    /// 然后从离线队列中获取对应的消息
    pub async fn get_by_pts_min(
        &self,
        user_id: u64,
        _min_pts: u64,
    ) -> Result<Vec<PushMessageRequest>, ServerError> {
        // 当前实现：获取所有消息，由调用方使用 UserMessageIndex 过滤
        // TODO: 优化为使用 Redis Sorted Set 存储 pts -> message 的映射
        self.get_all(user_id).await
    }

    /// 获取队列长度
    pub async fn len(&self, user_id: u64) -> Result<usize, ServerError> {
        let key = Self::queue_key(user_id);
        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        let len: usize = conn
            .llen(&key)
            .await
            .map_err(|e| ServerError::Internal(format!("LLEN 失败: {}", e)))?;

        Ok(len)
    }

    /// 清空队列（推送完成后）
    pub async fn clear(&self, user_id: u64) -> Result<(), ServerError> {
        let key = Self::queue_key(user_id);
        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        conn.del::<_, ()>(&key)
            .await
            .map_err(|e| ServerError::Internal(format!("DEL 失败: {}", e)))?;

        info!("🗑️ 清空离线消息队列: user={}", user_id);

        Ok(())
    }

    /// 检查队列是否为空
    pub async fn is_empty(&self, user_id: u64) -> Result<bool, ServerError> {
        let len = self.len(user_id).await?;
        Ok(len == 0)
    }

    /// 从离线队列中删除指定消息（根据 message_id）
    ///
    /// 用于撤回消息时，从未收到消息的用户的离线队列中删除该消息
    pub async fn remove_message_by_id(
        &self,
        user_id: u64,
        message_id: u64,
    ) -> Result<bool, ServerError> {
        let key = Self::queue_key(user_id);
        let mut conn = self
            .redis_client
            .get_multiplexed_tokio_connection()
            .await
            .map_err(|e| ServerError::Internal(format!("获取 Redis 连接失败: {}", e)))?;

        // 获取所有消息
        let values: Vec<String> = conn
            .lrange(&key, 0, -1)
            .await
            .map_err(|e| ServerError::Internal(format!("LRANGE 失败: {}", e)))?;

        let mut found = false;
        let mut filtered_values = Vec::new();

        // 过滤掉匹配的消息
        for value in values {
            if let Ok(msg) = serde_json::from_str::<PushMessageRequest>(&value) {
                if msg.server_message_id == message_id {
                    found = true;
                    info!(
                        "🗑️ 从离线队列删除消息: user={}, message_id={}",
                        user_id, message_id
                    );
                    continue; // 跳过这条消息
                }
            }
            filtered_values.push(value);
        }

        if found {
            // Pipeline: DEL + 重建列表 + LTRIM + EXPIRE 合并为单次 RTT
            let mut pipe = redis::pipe();
            pipe.del(&key).ignore();

            if !filtered_values.is_empty() {
                for value in filtered_values.iter().rev() {
                    pipe.lpush(&key, value).ignore();
                }
                let max_index = (self.max_queue_size - 1) as isize;
                pipe.ltrim(&key, 0, max_index).ignore();
                pipe.expire(&key, self.expire_seconds).ignore();
            }

            pipe.query_async::<()>(&mut conn)
                .await
                .map_err(|e| ServerError::Internal(format!("Pipeline 执行失败: {}", e)))?;
        }

        Ok(found)
    }

    /// 生成队列 key
    fn queue_key(user_id: u64) -> String {
        format!("offline:{}:messages", user_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 注意：这些测试需要 Redis 运行在 localhost:6379
    // 如果没有 Redis，测试会失败

    #[tokio::test]
    #[ignore] // 需要 Redis，默认忽略
    async fn test_push_and_get() {
        let service = OfflineQueueService::new("redis://127.0.0.1:6379").expect("连接 Redis 失败");

        let user_id = "test_user_001";

        // 清空队列
        service.clear(user_id).await.expect("清空失败");

        // 推送消息
        let msg = PushMessageRequest {
            pts: 1,
            message_id: "msg_001".to_string(),
            from_user_id: "alice".to_string(),
            to_user_id: user_id,
            channel_id: "channel_001".to_string(),
            local_message_id: "client_001".to_string(),
            message_type: "text".to_string(),
            content: "Hello".to_string(),
            metadata: None,
            created_at: 1704624000,
            is_revoked: false,
        };

        service.push(user_id, &msg).await.expect("推送失败");

        // 获取消息
        let messages = service.get_all(user_id).await.expect("获取失败");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].pts, 1);

        // 清空
        service.clear(user_id).await.expect("清空失败");
    }

    #[tokio::test]
    #[ignore]
    async fn test_max_queue_size() {
        let service = OfflineQueueService::new("redis://127.0.0.1:6379")
            .expect("连接 Redis 失败")
            .with_max_size(10); // 设置上限为 10

        let user_id = "test_user_002";
        service.clear(user_id).await.expect("清空失败");

        // 推送 15 条消息
        for i in 1..=15 {
            let msg = PushMessageRequest {
                pts: i,
                message_id: format!("msg_{:03}", i),
                from_user_id: "alice".to_string(),
                to_user_id: user_id,
                channel_id: "channel_001".to_string(),
                local_message_id: format!("client_{:03}", i),
                message_type: "text".to_string(),
                content: format!("Message {}", i),
                metadata: None,
                created_at: 1704624000 + i as i64,
                is_revoked: false,
            };
            service.push(user_id, &msg).await.expect("推送失败");
        }

        // 队列应该只保留最新 10 条（pts 6-15）
        let messages = service.get_all(user_id).await.expect("获取失败");
        assert_eq!(messages.len(), 10);
        assert_eq!(messages[0].pts, 15); // 最新的
        assert_eq!(messages[9].pts, 6); // 最旧的

        // 清空
        service.clear(user_id).await.expect("清空失败");
    }
}
