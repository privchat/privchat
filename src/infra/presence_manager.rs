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

use chrono::Utc;
use dashmap::DashMap;
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info};

use crate::error::ServerError;
use privchat_protocol::presence::{OnlineStatus, OnlineStatusInfo};

/// 私聊在线状态管理器
///
/// 功能：
/// 1. 管理用户之间的在线状态订阅关系
/// 2. 缓存用户的在线状态
/// 3. 推送在线状态变化通知
///
/// 设计原则：
/// - 按需订阅：只订阅当前关心的用户
/// - 自动清理：用户下线时自动取消所有订阅
/// - 内存友好：使用 DashMap + Moka Cache
pub struct PresenceManager {
    /// 订阅关系：target_user_id -> Set<subscriber_user_id>
    /// 记录谁在订阅某个用户的在线状态
    subscriptions: Arc<DashMap<u64, HashSet<u64>>>,

    /// 在线状态缓存：user_id -> OnlineStatusInfo
    /// TTL=5分钟，自动过期
    status_cache: Cache<u64, OnlineStatusInfo>,

    /// 最后活跃时间：user_id -> timestamp
    last_seen: Arc<DashMap<u64, i64>>,

    /// 配置
    config: PresenceConfig,
}

/// 在线状态管理器配置
#[derive(Debug, Clone)]
pub struct PresenceConfig {
    /// 在线阈值（秒）- 最近多久活跃算在线
    pub online_threshold_secs: i64,

    /// 状态缓存 TTL（秒）
    pub cache_ttl_secs: u64,

    /// 状态缓存容量
    pub cache_capacity: u64,

    /// 是否启用通知推送
    pub enable_push_notification: bool,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            online_threshold_secs: 180, // 3分钟
            cache_ttl_secs: 300,        // 5分钟
            cache_capacity: 100000,     // 10万用户
            enable_push_notification: true,
        }
    }
}

/// 在线状态统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceStats {
    /// 总订阅关系数（有多少个用户被订阅）
    pub total_subscriptions: usize,
    /// 总订阅者数（所有订阅者的总和）
    pub total_subscribers: usize,
    /// 缓存的状态数
    pub cached_statuses: u64,
    /// 记录的最后活跃时间数
    pub tracked_users: usize,
}

impl PresenceManager {
    /// 创建新的在线状态管理器
    pub fn new(config: PresenceConfig) -> Self {
        let status_cache = Cache::builder()
            .max_capacity(config.cache_capacity)
            .time_to_live(Duration::from_secs(config.cache_ttl_secs))
            .build();

        info!(
            "🔔 PresenceManager initialized: online_threshold={}s, cache_ttl={}s, capacity={}",
            config.online_threshold_secs, config.cache_ttl_secs, config.cache_capacity
        );

        Self {
            subscriptions: Arc::new(DashMap::new()),
            status_cache,
            last_seen: Arc::new(DashMap::new()),
            config,
        }
    }

    /// 使用默认配置创建
    pub fn with_default_config() -> Self {
        Self::new(PresenceConfig::default())
    }

    // ==================== 订阅管理 ====================

    /// 用户订阅对方的在线状态
    ///
    /// # 参数
    /// - subscriber_id: 订阅者（谁在订阅）
    /// - target_user_id: 目标用户（被订阅的用户）
    ///
    /// # 返回
    /// 返回目标用户的当前在线状态
    pub async fn subscribe(
        &self,
        subscriber_id: u64,
        target_user_id: u64,
    ) -> Result<OnlineStatusInfo, ServerError> {
        // 1. 添加订阅关系
        self.subscriptions
            .entry(target_user_id)
            .or_insert_with(HashSet::new)
            .insert(subscriber_id);

        debug!(
            "👁️ User {} subscribed to user {}'s presence",
            subscriber_id, target_user_id
        );

        // 2. 返回当前在线状态
        let status = self.get_status(target_user_id).await;
        Ok(status)
    }

    /// 取消订阅
    ///
    /// # 参数
    /// - subscriber_id: 订阅者
    /// - target_user_id: 目标用户
    pub fn unsubscribe(&self, subscriber_id: u64, target_user_id: u64) {
        if let Some(mut subscribers) = self.subscriptions.get_mut(&target_user_id) {
            subscribers.remove(&subscriber_id);

            debug!(
                "👁️ User {} unsubscribed from user {}'s presence",
                subscriber_id, target_user_id
            );

            // 如果没有订阅者了，移除整个条目
            if subscribers.is_empty() {
                drop(subscribers);
                self.subscriptions.remove(&target_user_id);
                debug!(
                    "🧹 Removed empty subscription entry for user {}",
                    target_user_id
                );
            }
        }
    }

    /// 用户下线时，取消其所有订阅
    ///
    /// # 参数
    /// - user_id: 下线的用户ID
    pub fn unsubscribe_all(&self, user_id: u64) {
        let mut removed_count = 0;

        // 遍历所有订阅关系，移除该用户
        for mut entry in self.subscriptions.iter_mut() {
            if entry.value_mut().remove(&user_id) {
                removed_count += 1;
            }
        }

        // 清理空的订阅关系
        self.subscriptions
            .retain(|_, subscribers| !subscribers.is_empty());

        if removed_count > 0 {
            debug!(
                "🧹 User {} offline: removed {} subscriptions",
                user_id, removed_count
            );
        }
    }

    /// 获取订阅者列表
    ///
    /// # 参数
    /// - user_id: 用户ID
    ///
    /// # 返回
    /// 订阅该用户的所有用户ID列表
    pub fn get_subscribers(&self, user_id: u64) -> Vec<u64> {
        self.subscriptions
            .get(&user_id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// 检查是否有人订阅某个用户
    pub fn has_subscribers(&self, user_id: u64) -> bool {
        self.subscriptions
            .get(&user_id)
            .map(|set| !set.is_empty())
            .unwrap_or(false)
    }

    // ==================== 状态管理 ====================

    /// 获取用户的在线状态
    ///
    /// # 参数
    /// - user_id: 用户ID
    ///
    /// # 返回
    /// 用户的在线状态信息
    pub async fn get_status(&self, user_id: u64) -> OnlineStatusInfo {
        // 先查缓存
        if let Some(cached) = self.status_cache.get(&user_id).await {
            return cached;
        }

        // 缓存未命中，计算状态
        let now = Utc::now().timestamp();
        let (status, last_seen_time) = if let Some(last_seen) = self.last_seen.get(&user_id) {
            let elapsed = now - *last_seen;
            let status = if elapsed <= self.config.online_threshold_secs {
                OnlineStatus::Online
            } else {
                OnlineStatus::from_elapsed_seconds(elapsed)
            };
            (status, *last_seen)
        } else {
            (OnlineStatus::LongTimeAgo, 0)
        };

        let info = OnlineStatusInfo {
            user_id,
            status: status.clone(),
            last_seen: last_seen_time,
            online_devices: vec![], // 简化版不追踪设备列表
        };

        // 写入缓存
        self.status_cache.insert(user_id, info.clone()).await;

        info
    }

    /// 批量获取在线状态（用于好友列表等场景）
    ///
    /// # 参数
    /// - user_ids: 用户ID列表
    ///
    /// # 返回
    /// user_id -> OnlineStatusInfo 的映射
    pub async fn batch_get_status(
        &self,
        user_ids: Vec<u64>,
    ) -> std::collections::HashMap<u64, OnlineStatusInfo> {
        let mut results = std::collections::HashMap::new();

        for user_id in user_ids {
            let status = self.get_status(user_id).await;
            results.insert(user_id, status);
        }

        results
    }

    /// 用户上线
    ///
    /// # 参数
    /// - user_id: 用户ID
    pub async fn user_online(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();

        // 获取旧状态
        let old_status = self.get_status(user_id).await.status;

        // 更新最后活跃时间
        self.last_seen.insert(user_id, now);

        // 更新缓存
        let new_info = OnlineStatusInfo {
            user_id,
            status: OnlineStatus::Online,
            last_seen: now,
            online_devices: vec![],
        };
        self.status_cache.insert(user_id, new_info.clone()).await;

        // 如果状态改变，通知订阅者
        if old_status != OnlineStatus::Online {
            debug!("🟢 User {} is now online", user_id);
            if self.config.enable_push_notification {
                self.notify_subscribers(user_id, new_info).await;
            }
        }

        Ok(())
    }

    /// 用户下线
    ///
    /// # 参数
    /// - user_id: 用户ID
    pub async fn user_offline(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();

        // 更新最后活跃时间
        self.last_seen.insert(user_id, now);

        // 更新缓存为最近在线
        let new_info = OnlineStatusInfo {
            user_id,
            status: OnlineStatus::Recently,
            last_seen: now,
            online_devices: vec![],
        };
        self.status_cache.insert(user_id, new_info.clone()).await;

        debug!("⚪ User {} is now offline", user_id);

        // 通知订阅者
        if self.config.enable_push_notification {
            self.notify_subscribers(user_id, new_info).await;
        }

        // 取消该用户的所有订阅
        self.unsubscribe_all(user_id);

        Ok(())
    }

    /// 更新用户心跳（用户活跃）
    ///
    /// # 参数
    /// - user_id: 用户ID
    pub async fn update_heartbeat(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();
        self.last_seen.insert(user_id, now);

        // 更新缓存中的时间戳，但不改变状态（避免频繁推送）
        if let Some(cached) = self.status_cache.get(&user_id).await {
            if cached.status == OnlineStatus::Online {
                let updated = OnlineStatusInfo {
                    user_id,
                    status: OnlineStatus::Online,
                    last_seen: now,
                    online_devices: cached.online_devices.clone(),
                };
                self.status_cache.insert(user_id, updated).await;
            }
        }

        Ok(())
    }

    // ==================== 通知推送 ====================

    /// 通知订阅者（状态变化）
    ///
    /// # 参数
    /// - user_id: 状态改变的用户ID
    /// - status_info: 新的状态信息
    async fn notify_subscribers(&self, user_id: u64, status_info: OnlineStatusInfo) {
        // 获取订阅者列表
        let subscribers = self.get_subscribers(user_id);

        if subscribers.is_empty() {
            return;
        }

        debug!(
            "📢 Notifying {} subscribers about user {} status change to {:?}",
            subscribers.len(),
            user_id,
            status_info.status
        );

        // TODO: 这里需要实现实际的推送逻辑
        // 可以通过消息队列或直接推送给在线用户
        // 暂时只打印日志
        for subscriber_id in subscribers {
            debug!(
                "  → Will notify user {} about user {} status: {:?}",
                subscriber_id, user_id, status_info.status
            );
        }
    }

    // ==================== 统计和清理 ====================

    /// 获取统计信息
    pub fn get_stats(&self) -> PresenceStats {
        let total_subscribers: usize = self
            .subscriptions
            .iter()
            .map(|entry| entry.value().len())
            .sum();

        PresenceStats {
            total_subscriptions: self.subscriptions.len(),
            total_subscribers,
            cached_statuses: self.status_cache.entry_count(),
            tracked_users: self.last_seen.len(),
        }
    }

    /// 清理过期数据
    ///
    /// 清理很久没活跃的用户的 last_seen 记录
    /// （缓存会自动过期，不需要手动清理）
    pub fn cleanup_stale_data(&self, threshold_days: i64) {
        let now = Utc::now().timestamp();
        let threshold_secs = threshold_days * 86400;

        let mut removed_count = 0;

        // 移除很久没活跃的用户记录
        self.last_seen.retain(|_, last_seen| {
            let elapsed = now - *last_seen;
            if elapsed > threshold_secs {
                removed_count += 1;
                false
            } else {
                true
            }
        });

        if removed_count > 0 {
            info!("🧹 Cleaned up {} stale user records", removed_count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_subscribe_and_get_status() {
        let manager = PresenceManager::with_default_config();

        // 用户1订阅用户2
        let status = manager.subscribe(1, 2).await.unwrap();

        // 初始状态应该是 LongTimeAgo
        assert_eq!(status.status, OnlineStatus::LongTimeAgo);

        // 检查订阅关系
        let subscribers = manager.get_subscribers(2);
        assert_eq!(subscribers.len(), 1);
        assert!(subscribers.contains(&1));
    }

    #[tokio::test]
    async fn test_user_online_offline() {
        let manager = PresenceManager::with_default_config();

        // 用户2上线
        manager.user_online(2).await.unwrap();

        // 检查状态
        let status = manager.get_status(2).await;
        assert_eq!(status.status, OnlineStatus::Online);

        // 用户2下线
        manager.user_offline(2).await.unwrap();

        // 检查状态
        let status = manager.get_status(2).await;
        assert_eq!(status.status, OnlineStatus::Recently);
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let manager = PresenceManager::with_default_config();

        // 用户1订阅用户2
        manager.subscribe(1, 2).await.unwrap();
        assert_eq!(manager.get_subscribers(2).len(), 1);

        // 取消订阅
        manager.unsubscribe(1, 2);
        assert_eq!(manager.get_subscribers(2).len(), 0);
    }

    #[tokio::test]
    async fn test_unsubscribe_all() {
        let manager = PresenceManager::with_default_config();

        // 用户1订阅用户2、3、4
        manager.subscribe(1, 2).await.unwrap();
        manager.subscribe(1, 3).await.unwrap();
        manager.subscribe(1, 4).await.unwrap();

        // 用户1下线，应该取消所有订阅
        manager.unsubscribe_all(1);

        assert_eq!(manager.get_subscribers(2).len(), 0);
        assert_eq!(manager.get_subscribers(3).len(), 0);
        assert_eq!(manager.get_subscribers(4).len(), 0);
    }

    #[tokio::test]
    async fn test_batch_get_status() {
        let manager = PresenceManager::with_default_config();

        // 设置一些用户上线
        manager.user_online(1).await.unwrap();
        manager.user_online(2).await.unwrap();

        // 批量查询
        let statuses = manager.batch_get_status(vec![1, 2, 3]).await;

        assert_eq!(statuses.len(), 3);
        assert_eq!(statuses[&1].status, OnlineStatus::Online);
        assert_eq!(statuses[&2].status, OnlineStatus::Online);
        assert_eq!(statuses[&3].status, OnlineStatus::LongTimeAgo);
    }

    #[tokio::test]
    async fn test_stats() {
        let manager = PresenceManager::with_default_config();

        // 创建一些订阅关系
        manager.subscribe(1, 2).await.unwrap();
        manager.subscribe(3, 2).await.unwrap();
        manager.subscribe(1, 4).await.unwrap();

        let stats = manager.get_stats();

        assert_eq!(stats.total_subscriptions, 2); // 用户2和4被订阅
        assert_eq!(stats.total_subscribers, 3); // 总共3个订阅关系
    }
}
