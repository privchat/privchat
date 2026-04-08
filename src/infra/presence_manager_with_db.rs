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
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, error, info};

use crate::error::ServerError;
use crate::repository::PresenceRepository;
use privchat_protocol::presence::{OnlineStatus, OnlineStatusInfo};

/// Presence 运行态状态存储（带数据库持久化）
///
/// 三层存储架构：
/// 1. L1: 内存 (DashMap) - 实时数据，最快
/// 2. L2: Redis (可选) - 热数据，7天TTL
/// 3. L3: 数据库 (MySQL) - 冷数据，30天保留
///
/// 该类型是 Presence v1 的低层 runtime store：
/// - 维护 user_id -> last_seen / status 的底层状态
/// - 提供 batch_status 所需的直接查询能力
/// - 为 PresenceTracker 提供 connect / disconnect / heartbeat 的事实落地
///
/// 注意：
/// - 它不是 Presence v1 的对外业务入口
/// - channel fanout / batch_status 判权 / realtime delivery 均不在这里处理
pub struct PresenceManagerWithDb {
    /// 在线状态缓存：user_id -> OnlineStatusInfo
    status_cache: Cache<u64, OnlineStatusInfo>,

    /// 最后活跃时间（内存）：user_id -> timestamp
    last_seen: Arc<DashMap<u64, i64>>,

    /// 数据库仓库
    db_repo: Option<Arc<PresenceRepository>>,

    /// 配置
    config: PresenceConfig,

    /// 待批量更新的标记
    pending_updates: Arc<DashMap<u64, i64>>,
}

/// 在线状态管理器配置
#[derive(Debug, Clone)]
pub struct PresenceConfig {
    /// 在线阈值（秒）
    pub online_threshold_secs: i64,

    /// 状态缓存 TTL（秒）
    pub cache_ttl_secs: u64,

    /// 状态缓存容量
    pub cache_capacity: u64,

    /// 是否启用通知推送
    pub enable_push_notification: bool,

    /// 批量更新间隔（秒）
    pub batch_update_interval_secs: u64,

    /// 数据保留天数
    pub retention_days: u32,

    /// 清理任务间隔（秒）
    pub cleanup_interval_secs: u64,
}

impl Default for PresenceConfig {
    fn default() -> Self {
        Self {
            online_threshold_secs: 180, // 3分钟
            cache_ttl_secs: 300,        // 5分钟
            cache_capacity: 100000,     // 10万用户
            enable_push_notification: true,
            batch_update_interval_secs: 300, // 5分钟
            retention_days: 30,              // 保留30天
            cleanup_interval_secs: 86400,    // 每天清理一次
        }
    }
}

/// 在线状态统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PresenceStats {
    pub cached_statuses: u64,
    pub tracked_users: usize,
    pub pending_db_updates: usize,
}

impl PresenceManagerWithDb {
    /// 创建新的在线状态管理器（带数据库）
    pub fn new(config: PresenceConfig, db_repo: Option<Arc<PresenceRepository>>) -> Arc<Self> {
        let status_cache = Cache::builder()
            .max_capacity(config.cache_capacity)
            .time_to_live(Duration::from_secs(config.cache_ttl_secs))
            .build();

        info!(
            "🔔 PresenceStateStore initialized: online_threshold={}s, cache_ttl={}s, capacity={}, db={}",
            config.online_threshold_secs, config.cache_ttl_secs, config.cache_capacity,
            if db_repo.is_some() { "enabled" } else { "disabled" }
        );

        let manager = Arc::new(Self {
            status_cache,
            last_seen: Arc::new(DashMap::new()),
            db_repo: db_repo.clone(),
            config: config.clone(),
            pending_updates: Arc::new(DashMap::new()),
        });

        // 启动后台任务
        if db_repo.is_some() {
            Self::start_batch_update_task(manager.clone());
            Self::start_cleanup_task(manager.clone());
        }

        manager
    }

    /// 使用默认配置创建
    pub fn with_default_config(db_repo: Option<Arc<PresenceRepository>>) -> Arc<Self> {
        Self::new(PresenceConfig::default(), db_repo)
    }

    // ==================== 状态管理 ====================

    /// 获取用户的在线状态（三层查询）
    ///
    /// 查询顺序：
    /// 1. L1: 内存 (DashMap) - 最快
    /// 2. L2: Redis (TODO) - 快
    /// 3. L3: 数据库 (MySQL) - 慢但完整
    pub async fn get_status_with_db(&self, user_id: u64) -> OnlineStatusInfo {
        // 先查缓存
        if let Some(cached) = self.status_cache.get(&user_id).await {
            return cached;
        }

        let now = Utc::now().timestamp();

        // 1. 查询内存
        if let Some(last_seen) = self.last_seen.get(&user_id) {
            return self.calculate_status(user_id, *last_seen, now).await;
        }

        // 2. TODO: 查询 Redis（可选）

        // 3. 查询数据库
        if let Some(ref db_repo) = self.db_repo {
            if let Ok(Some(last_seen)) = db_repo.get_last_seen(user_id).await {
                // 回填内存缓存
                self.last_seen.insert(user_id, last_seen);
                return self.calculate_status(user_id, last_seen, now).await;
            }
        }

        // 没有任何记录
        let info = OnlineStatusInfo {
            user_id,
            status: OnlineStatus::LongTimeAgo,
            last_seen: 0,
            online_devices: vec![],
        };

        self.status_cache.insert(user_id, info.clone()).await;
        info
    }

    /// 计算并缓存在线状态
    async fn calculate_status(&self, user_id: u64, last_seen: i64, now: i64) -> OnlineStatusInfo {
        let elapsed = now - last_seen;
        let status = if elapsed <= self.config.online_threshold_secs {
            OnlineStatus::Online
        } else {
            OnlineStatus::from_elapsed_seconds(elapsed)
        };

        let info = OnlineStatusInfo {
            user_id,
            status: status.clone(),
            last_seen,
            online_devices: vec![],
        };

        self.status_cache.insert(user_id, info.clone()).await;
        info
    }

    /// 批量获取在线状态
    pub async fn batch_get_status(
        &self,
        user_ids: Vec<u64>,
    ) -> std::collections::HashMap<u64, OnlineStatusInfo> {
        let mut results = std::collections::HashMap::new();

        for user_id in user_ids {
            let status = self.get_status_with_db(user_id).await;
            results.insert(user_id, status);
        }

        results
    }

    /// 用户上线
    pub async fn user_online(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();

        // 获取旧状态
        let old_status = self.get_status_with_db(user_id).await.status;

        // 检查是否是首次上线（内存和数据库中都没有）
        let is_first_time = !self.last_seen.contains_key(&user_id);

        // 更新内存
        self.last_seen.insert(user_id, now);

        // 立即更新数据库的情况：
        // 1. 用户上线（重要事件）
        // 2. 首次上线（必须立即持久化）
        if let Some(ref db_repo) = self.db_repo {
            if is_first_time {
                // 首次上线，立即写入数据库
                let db_repo_clone = Arc::clone(db_repo);
                tokio::spawn(async move {
                    if let Err(e) = db_repo_clone.update_last_seen(user_id, now).await {
                        error!(
                            "Failed to update last_seen for first-time user {}: {}",
                            user_id, e
                        );
                    } else {
                        debug!("✅ First-time user {} last_seen saved to DB", user_id);
                    }
                });
            } else {
                // 非首次上线，标记为待更新（批量更新任务会处理）
                self.pending_updates.insert(user_id, now);

                // 但用户上线仍然是重要事件，异步立即更新
                let db_repo_clone = Arc::clone(db_repo);
                tokio::spawn(async move {
                    if let Err(e) = db_repo_clone.update_last_seen(user_id, now).await {
                        error!("Failed to update last_seen for user {}: {}", user_id, e);
                    }
                });
            }
        } else {
            // 没有数据库，只标记为待更新
            self.pending_updates.insert(user_id, now);
        }

        // 更新缓存
        let new_info = OnlineStatusInfo {
            user_id,
            status: OnlineStatus::Online,
            last_seen: now,
            online_devices: vec![],
        };
        self.status_cache.insert(user_id, new_info.clone()).await;

        if old_status != OnlineStatus::Online {
            debug!(
                "🟢 PresenceStateStore: user {} is now online (first_time={})",
                user_id, is_first_time
            );
        }

        Ok(())
    }

    /// 用户下线
    pub async fn user_offline(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();

        // 更新内存
        self.last_seen.insert(user_id, now);

        // 立即更新数据库（用户下线是重要事件，必须持久化）
        if let Some(ref db_repo) = self.db_repo {
            if let Err(e) = db_repo.update_last_seen(user_id, now).await {
                error!(
                    "Failed to update last_seen on offline for user {}: {}",
                    user_id, e
                );
            }
        }

        // 更新缓存
        let new_info = OnlineStatusInfo {
            user_id,
            status: OnlineStatus::Recently,
            last_seen: now,
            online_devices: vec![],
        };
        self.status_cache.insert(user_id, new_info.clone()).await;

        debug!("⚪ PresenceStateStore: user {} is now offline", user_id);

        Ok(())
    }

    /// 更新心跳（只更新内存，由批量任务更新数据库）
    pub async fn update_heartbeat(&self, user_id: u64) -> Result<(), ServerError> {
        let now = Utc::now().timestamp();
        self.last_seen.insert(user_id, now);

        // 标记为待更新
        self.pending_updates.insert(user_id, now);

        // 更新缓存中的时间戳（但不改变状态，避免频繁推送）
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

    // ==================== 后台任务 ====================

    /// 启动批量更新任务
    fn start_batch_update_task(manager: Arc<Self>) {
        let interval_secs = manager.config.batch_update_interval_secs;

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(interval_secs));

            loop {
                ticker.tick().await;

                if let Err(e) = manager.batch_update_db().await {
                    error!("Batch update task error: {}", e);
                }
            }
        });

        info!("🔄 Started batch update task (interval={}s)", interval_secs);
    }

    /// 批量更新数据库
    async fn batch_update_db(&self) -> Result<(), ServerError> {
        if self.db_repo.is_none() {
            return Ok(());
        }

        // 收集所有待更新的数据
        let updates: Vec<(u64, i64)> = self
            .pending_updates
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();

        if updates.is_empty() {
            return Ok(());
        }

        // 批量更新
        if let Some(ref db_repo) = self.db_repo {
            db_repo.batch_update_last_seen(&updates).await?;

            // 清空已更新的记录
            for (user_id, _) in &updates {
                self.pending_updates.remove(user_id);
            }

            debug!("✅ Batch updated {} last_seen records", updates.len());
        }

        Ok(())
    }

    /// 启动清理任务
    fn start_cleanup_task(manager: Arc<Self>) {
        let cleanup_interval_secs = manager.config.cleanup_interval_secs;
        let retention_days = manager.config.retention_days;

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(cleanup_interval_secs));

            loop {
                ticker.tick().await;

                if let Err(e) = manager.cleanup_expired_data(retention_days).await {
                    error!("Cleanup task error: {}", e);
                }
            }
        });

        info!(
            "🧹 Started cleanup task (interval={}s, retention={}days)",
            cleanup_interval_secs, retention_days
        );
    }

    /// 清理过期数据
    async fn cleanup_expired_data(&self, retention_days: u32) -> Result<(), ServerError> {
        if let Some(ref db_repo) = self.db_repo {
            let deleted = db_repo.cleanup_expired(retention_days, 10000).await?;
            if deleted > 0 {
                info!("🧹 Cleaned up {} expired last_seen records", deleted);
            }
        }
        Ok(())
    }

    // ==================== 统计信息 ====================

    pub fn get_stats(&self) -> PresenceStats {
        PresenceStats {
            cached_statuses: self.status_cache.entry_count(),
            tracked_users: self.last_seen.len(),
            pending_db_updates: self.pending_updates.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_user_online_offline_without_db() {
        let manager = PresenceManagerWithDb::with_default_config(None);

        manager.user_online(2).await.unwrap();
        let status = manager.get_status_with_db(2).await;
        assert_eq!(status.status, OnlineStatus::Online);

        manager.user_offline(2).await.unwrap();
        let status = manager.get_status_with_db(2).await;
        assert_eq!(status.status, OnlineStatus::Recently);
    }
}
