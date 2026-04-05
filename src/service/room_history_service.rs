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

use std::sync::Arc;

use tracing::{debug, warn};

use crate::config::RoomConfig;
use crate::error::{Result, ServerError};
use crate::infra::redis::RedisClient;

/// 防止配置异常导致单次回放过大
const MAX_SUBSCRIBE_HISTORY_LIMIT: usize = 200;

/// Room 订阅历史服务（Redis 列表）
pub struct RoomHistoryService {
    redis: Arc<RedisClient>,
    config: RoomConfig,
}

impl RoomHistoryService {
    pub fn new(redis: Arc<RedisClient>, config: RoomConfig) -> Self {
        Self { redis, config }
    }

    /// 是否启用订阅后历史推送
    pub fn subscribe_history_enabled(&self) -> bool {
        self.config.subscribe_history && self.subscribe_history_limit() > 0
    }

    /// 实际生效的历史条数（带保护上限）
    pub fn subscribe_history_limit(&self) -> usize {
        self.config
            .subscribe_history_limit
            .min(MAX_SUBSCRIBE_HISTORY_LIMIT)
    }

    /// 写入一条 room 广播到历史列表，并按配置截断
    pub async fn append_history(
        &self,
        channel_id: u64,
        publish_request: &privchat_protocol::protocol::PublishRequest,
    ) -> Result<()> {
        if !self.subscribe_history_enabled() {
            return Ok(());
        }

        let payload = serde_json::to_string(publish_request).map_err(|e| {
            ServerError::Internal(format!(
                "Room 历史序列化失败: channel_id={}, error={}",
                channel_id, e
            ))
        })?;

        let key = Self::history_key(channel_id);
        let limit = self.subscribe_history_limit();
        self.redis.lpush(&key, &payload).await?;
        self.redis
            .ltrim(&key, 0, limit.saturating_sub(1) as isize)
            .await?;

        debug!(
            "📚 RoomHistory: appended channel_id={}, limit={}",
            channel_id, limit
        );
        Ok(())
    }

    /// 获取频道近期历史（按时间正序，旧 -> 新）
    pub async fn list_recent_history(
        &self,
        channel_id: u64,
    ) -> Result<Vec<privchat_protocol::protocol::PublishRequest>> {
        if !self.subscribe_history_enabled() {
            return Ok(Vec::new());
        }

        let key = Self::history_key(channel_id);
        let limit = self.subscribe_history_limit();
        let raw_items = self
            .redis
            .lrange(&key, 0, limit.saturating_sub(1) as isize)
            .await?;

        let mut items = Vec::with_capacity(raw_items.len());
        for raw in raw_items.into_iter().rev() {
            match serde_json::from_str::<privchat_protocol::protocol::PublishRequest>(&raw) {
                Ok(item) => items.push(item),
                Err(e) => {
                    warn!(
                        "⚠️ RoomHistory: 反序列化失败 channel_id={}, error={}",
                        channel_id, e
                    );
                }
            }
        }

        Ok(items)
    }

    fn history_key(channel_id: u64) -> String {
        format!("room:history:{}", channel_id)
    }
}
