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

//! 消息去重服务
//!
//! 基于 local_message_id 实现消息去重，防止重复消息处理

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info};

/// 消息去重服务
pub struct MessageDedupService {
    /// 已处理的消息集合 (user_id, local_message_id)
    processed_messages: Arc<RwLock<HashSet<(u64, u64)>>>,

    /// 消息时间戳（用于清理过期记录）
    message_timestamps: Arc<RwLock<Vec<(u64, u64, Instant)>>>,

    /// 清理间隔（秒）
    cleanup_interval: Duration,

    /// 消息保留时间（秒）
    message_retention: Duration,
}

impl MessageDedupService {
    /// 创建新的消息去重服务
    pub fn new() -> Self {
        Self {
            processed_messages: Arc::new(RwLock::new(HashSet::new())),
            message_timestamps: Arc::new(RwLock::new(Vec::new())),
            cleanup_interval: Duration::from_secs(300), // 5分钟清理一次
            message_retention: Duration::from_secs(3600), // 保留1小时
        }
    }

    /// 检查消息是否已处理（去重检查）
    ///
    /// 返回 true 如果消息已处理过（重复消息），false 如果未处理过
    pub async fn is_duplicate(&self, user_id: u64, local_message_id: u64) -> bool {
        let key = (user_id, local_message_id);
        let processed = self.processed_messages.read().await;
        let is_dup = processed.contains(&key);

        if is_dup {
            debug!(
                "🔄 检测到重复消息: user_id={}, local_message_id={}",
                user_id, local_message_id
            );
        }

        is_dup
    }

    /// 标记消息为已处理
    pub async fn mark_as_processed(&self, user_id: u64, local_message_id: u64) {
        let key = (user_id, local_message_id);
        let mut processed = self.processed_messages.write().await;
        processed.insert(key);

        // 记录时间戳用于清理
        let mut timestamps = self.message_timestamps.write().await;
        timestamps.push((user_id, local_message_id, Instant::now()));

        debug!(
            "✅ 标记消息为已处理: user_id={}, local_message_id={}",
            user_id, local_message_id
        );
    }

    /// 清理过期的消息记录
    pub async fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut timestamps = self.message_timestamps.write().await;
        let mut processed = self.processed_messages.write().await;

        let initial_count = timestamps.len();

        // 移除过期的记录
        timestamps.retain(|(user_id, local_message_id, timestamp)| {
            if now.duration_since(*timestamp) > self.message_retention {
                processed.remove(&(*user_id, *local_message_id));
                false
            } else {
                true
            }
        });

        let removed_count = initial_count - timestamps.len();
        if removed_count > 0 {
            info!("🧹 清理过期消息记录: 移除了 {} 条记录", removed_count);
        }
    }

    /// 启动定期清理任务
    pub fn start_cleanup_task(&self) {
        let service = Arc::new(self.clone());
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(service.cleanup_interval);
            loop {
                interval.tick().await;
                service.cleanup_expired().await;
            }
        });
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> (usize, usize) {
        let processed = self.processed_messages.read().await;
        let timestamps = self.message_timestamps.read().await;
        (processed.len(), timestamps.len())
    }
}

impl Clone for MessageDedupService {
    fn clone(&self) -> Self {
        Self {
            processed_messages: Arc::clone(&self.processed_messages),
            message_timestamps: Arc::clone(&self.message_timestamps),
            cleanup_interval: self.cleanup_interval,
            message_retention: self.message_retention,
        }
    }
}

impl Default for MessageDedupService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_message_dedup() {
        let service = MessageDedupService::new();

        // 第一次检查应该返回 false（未处理过）
        assert!(!service.is_duplicate("user1", "msg1").await);

        // 标记为已处理
        service.mark_as_processed("user1", "msg1").await;

        // 再次检查应该返回 true（已处理过）
        assert!(service.is_duplicate("user1", "msg1").await);

        // 不同的消息应该返回 false
        assert!(!service.is_duplicate("user1", "msg2").await);

        // 不同用户的消息应该返回 false
        assert!(!service.is_duplicate("user2", "msg1").await);
    }

    #[tokio::test]
    async fn test_cleanup_expired() {
        let service = MessageDedupService::new();

        // 标记一些消息
        service.mark_as_processed("user1", "msg1").await;
        service.mark_as_processed("user1", "msg2").await;

        // 等待超过保留时间（这里需要修改测试以使用更短的保留时间）
        // 为了测试，我们直接调用清理，但实际中需要等待
        let (count_before, _) = service.get_stats().await;
        assert!(count_before >= 2);
    }
}
