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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use privchat_protocol::protocol::PushMessageRequest;

fn parse_session_id(raw: &str) -> Result<msgtrans::SessionId> {
    let id = raw
        .strip_prefix("session-")
        .unwrap_or(raw)
        .parse::<u64>()
        .map_err(|e| anyhow::anyhow!("invalid session id '{}': {}", raw, e))?;
    Ok(msgtrans::SessionId::from(id))
}

/// 用户ID类型
pub type UserId = u64;
/// 会话ID类型
pub type SessionId = String;
/// 消息ID类型
pub type MessageId = u64;
/// 设备ID类型
pub type DeviceId = String; // 保留UUID字符串

/// 用户在线状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserOnlineStatus {
    /// 用户ID
    pub user_id: UserId,
    /// 在线设备列表
    pub devices: Vec<DeviceSession>,
    /// 最后活跃时间
    pub last_active: u64,
}

/// 设备会话信息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceSession {
    /// 设备ID
    pub device_id: DeviceId,
    /// 会话ID
    pub session_id: SessionId,
    /// 设备类型
    pub device_type: String,
    /// 上线时间
    pub online_time: u64,
    /// 最后活跃时间
    pub last_active: u64,
    /// 设备状态
    pub status: DeviceStatus,
}

/// 设备状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum DeviceStatus {
    #[default]
    Online,
    Away,
    Busy,
    Offline,
}

/// 离线消息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OfflineMessage {
    /// 消息ID
    pub message_id: MessageId,
    /// 目标用户ID
    pub target_user_id: UserId,
    /// 目标设备ID（可选，如果指定则只发送给特定设备）
    pub target_device_id: Option<DeviceId>,
    /// 消息内容
    pub message: PushMessageRequest,
    /// 创建时间
    pub created_at: u64,
    /// 过期时间
    pub expires_at: u64,
    /// 重试次数
    pub retry_count: u32,
    /// 优先级
    pub priority: MessagePriority,
}

/// 消息优先级
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum MessagePriority {
    Low = 0,
    #[default]
    Normal = 1,
    High = 2,
    Urgent = 3,
}

/// 消息路由配置
#[derive(Debug, Clone)]
pub struct MessageRouterConfig {
    /// 离线消息最大保留时间（秒）
    pub offline_message_ttl: u64,
    /// 离线消息最大重试次数
    pub max_retry_count: u32,
    /// 批量推送最大消息数
    pub max_batch_size: usize,
    /// 消息路由超时时间（毫秒）
    pub route_timeout_ms: u64,
    /// 离线队列最大长度
    pub max_offline_queue_size: usize,
}

impl Default for MessageRouterConfig {
    fn default() -> Self {
        Self {
            offline_message_ttl: 7 * 24 * 3600, // 7天
            max_retry_count: 3,
            max_batch_size: 100,
            route_timeout_ms: 5000, // 5秒
            max_offline_queue_size: 10000,
        }
    }
}

/// 消息路由结果
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// 成功路由的设备数量
    pub success_count: usize,
    /// 失败路由的设备数量
    pub failed_count: usize,
    /// 离线设备数量
    pub offline_count: usize,
    /// 路由延迟（毫秒）
    pub latency_ms: u64,
}

/// 消息路由器
///
/// 投递 / 离线判定的**唯一真源**是 [`crate::infra::ConnectionManager`]（spec §1）。
pub struct MessageRouter {
    config: MessageRouterConfig,
    connection_manager: Arc<crate::infra::ConnectionManager>,
    stats: Arc<RwLock<MessageRouterStats>>,
}

/// 消息路由统计
#[derive(Debug, Default)]
pub struct MessageRouterStats {
    /// 总消息数
    pub total_messages: u64,
    /// 成功路由数
    pub success_routes: u64,
    /// 失败路由数
    pub failed_routes: u64,
    /// 离线消息数
    pub offline_messages: u64,
    /// 平均延迟
    pub avg_latency_ms: f64,
}

impl MessageRouter {
    pub fn new(
        config: MessageRouterConfig,
        connection_manager: Arc<crate::infra::ConnectionManager>,
    ) -> Self {
        Self {
            config,
            connection_manager,
            stats: Arc::new(RwLock::new(MessageRouterStats::default())),
        }
    }

    /// 路由消息到指定用户（spec §6.1 热路径）。
    ///
    /// 投递与离线判定**唯一**依赖 [`ConnectionManager::send_push_to_user`]（A→B 二次过滤）。
    /// - `success_count >= 1` → 不写离线队列
    /// - `success_count == 0` → 返回 `offline_count = 1`，由调用方写离线队列
    pub async fn route_message_to_user(
        &self,
        user_id: &UserId,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        let success_count = self
            .connection_manager
            .send_push_to_user(*user_id, &message)
            .await?;
        let elapsed = start_time.elapsed().unwrap_or_default();
        Ok(RouteResult {
            success_count,
            failed_count: 0,
            offline_count: if success_count == 0 { 1 } else { 0 },
            latency_ms: elapsed.as_millis() as u64,
        })
    }

    /// 路由消息到指定用户的特定设备（spec §6.1 设备级过滤）。
    pub async fn route_message_to_device(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        let success_count = self
            .connection_manager
            .send_push_to_device(*user_id, device_id, &message)
            .await?;
        let elapsed = start_time.elapsed().unwrap_or_default();
        Ok(RouteResult {
            success_count,
            failed_count: 0,
            offline_count: if success_count == 0 { 1 } else { 0 },
            latency_ms: elapsed.as_millis() as u64,
        })
    }

    /// 直接按 `session_id` 路由消息（spec §6.1 B 单点过滤）。
    pub async fn route_message_to_session(
        &self,
        session_id: &SessionId,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        let parsed = match parse_session_id(session_id) {
            Ok(sid) => sid,
            Err(e) => {
                warn!(
                    "⚠️ route_message_to_session parse session_id failed: {} err={}",
                    session_id, e
                );
                let elapsed = start_time.elapsed().unwrap_or_default();
                return Ok(RouteResult {
                    success_count: 0,
                    failed_count: 1,
                    offline_count: 0,
                    latency_ms: elapsed.as_millis() as u64,
                });
            }
        };
        let success_count = self
            .connection_manager
            .send_push_to_session(parsed, &message)
            .await?;
        let elapsed = start_time.elapsed().unwrap_or_default();
        Ok(RouteResult {
            success_count,
            failed_count: 0,
            offline_count: 0,
            latency_ms: elapsed.as_millis() as u64,
        })
    }

    /// 批量路由消息到多个用户
    pub async fn route_message_to_users(
        &self,
        user_ids: &[UserId],
        message: PushMessageRequest,
    ) -> Result<HashMap<UserId, RouteResult>> {
        let mut results = HashMap::new();

        // 并发路由到所有用户
        let futures = user_ids.iter().map(|user_id| {
            let router = self;
            let message = message.clone();
            let user_id = user_id.clone();
            async move {
                let result = router.route_message_to_user(&user_id, message).await;
                (user_id, result)
            }
        });

        let route_results = futures::future::join_all(futures).await;

        for (user_id, result) in route_results {
            results.insert(
                user_id,
                result.unwrap_or_else(|_| RouteResult {
                    success_count: 0,
                    failed_count: 1,
                    offline_count: 0,
                    latency_ms: 0,
                }),
            );
        }

        Ok(results)
    }

    /// 获取路由统计信息
    pub async fn get_stats(&self) -> MessageRouterStats {
        let stats = self.stats.read().await;
        MessageRouterStats {
            total_messages: stats.total_messages,
            success_routes: stats.success_routes,
            failed_routes: stats.failed_routes,
            offline_messages: stats.offline_messages,
            avg_latency_ms: stats.avg_latency_ms,
        }
    }
}
