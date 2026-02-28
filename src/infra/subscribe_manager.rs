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

//! Subscribe Manager - 频道事件订阅管理
//!
//! 管理 session 对频道的事件订阅（typing / presence / read_receipt 等）
//! - 订阅绑定到 session_id，不是 user_id
//! - 每个 session 可同时订阅多个频道
//! - 推送目标是 session_id，不是 user_id

use dashmap::DashMap;
use msgtrans::SessionId;
use std::collections::HashSet;
use tracing::{debug, info};

pub struct SubscribeManager {
    /// channel_id -> Vec<session_id>
    channel_routes: DashMap<u64, Vec<SessionId>>,
    /// session_id -> Set<channel_id>（一个 session 可订阅多个频道）
    session_channels: DashMap<SessionId, HashSet<u64>>,
}

impl SubscribeManager {
    pub fn new() -> Self {
        Self {
            channel_routes: DashMap::new(),
            session_channels: DashMap::new(),
        }
    }

    /// 订阅频道
    /// 返回 Ok(()) 或 Err(reason) 如果频道已满或 session 订阅数超限
    pub fn subscribe(&self, session_id: SessionId, channel_id: u64) -> Result<(), &'static str> {
        // 检查是否已订阅该频道（幂等）
        if let Some(channels) = self.session_channels.get(&session_id) {
            if channels.contains(&channel_id) {
                debug!(
                    "Session {} already subscribed to channel {}",
                    session_id, channel_id
                );
                return Ok(());
            }
            // 检查 session 订阅数上限（防止泄漏）
            if channels.len() >= MAX_SUBSCRIPTIONS_PER_SESSION {
                info!(
                    "Session {} reached subscription limit ({}/{}), rejecting channel {}",
                    session_id, channels.len(), MAX_SUBSCRIPTIONS_PER_SESSION, channel_id
                );
                return Err("too many subscriptions");
            }
        }

        // 检查频道在线人数限制
        let current_count = self.channel_routes.get(&channel_id).map(|s| s.len()).unwrap_or(0);
        if current_count >= MAX_CHANNEL_SUBSCRIBERS_ONLINE {
            info!(
                "Channel {} is full ({}/{}), rejecting session {}",
                channel_id, current_count, MAX_CHANNEL_SUBSCRIBERS_ONLINE, session_id
            );
            return Err("channel is full");
        }

        // 加入频道
        self.channel_routes
            .entry(channel_id)
            .or_insert_with(Vec::new)
            .push(session_id.clone());

        // 更新 session -> channels 映射
        self.session_channels
            .entry(session_id.clone())
            .or_insert_with(HashSet::new)
            .insert(channel_id);

        info!(
            "Session {} subscribed to channel {}",
            session_id, channel_id
        );

        Ok(())
    }

    /// 取消订阅指定频道
    pub fn unsubscribe(&self, session_id: &SessionId, channel_id: u64) -> bool {
        // 从 session_channels 中移除
        let mut removed = false;
        if let Some(mut channels) = self.session_channels.get_mut(session_id) {
            removed = channels.remove(&channel_id);
            if channels.is_empty() {
                drop(channels);
                self.session_channels.remove(session_id);
            }
        }

        // 从 channel_routes 中移除
        if removed {
            self.unsubscribe_from_channel(session_id, channel_id);
            info!("Session {} unsubscribed from channel {}", session_id, channel_id);
        }

        removed
    }

    /// 从指定频道移除 session
    fn unsubscribe_from_channel(&self, session_id: &SessionId, channel_id: u64) {
        if let Some(mut sessions) = self.channel_routes.get_mut(&channel_id) {
            sessions.retain(|s| s != session_id);
            if sessions.is_empty() {
                drop(sessions);
                self.channel_routes.remove(&channel_id);
            }
        }
    }

    /// 连接断开时清理所有订阅
    pub fn on_session_disconnect(&self, session_id: &SessionId) -> Vec<u64> {
        let channels = self.session_channels.remove(session_id)
            .map(|(_, channels)| channels)
            .unwrap_or_default();

        for channel_id in &channels {
            self.unsubscribe_from_channel(session_id, *channel_id);
        }

        let result: Vec<u64> = channels.into_iter().collect();
        if !result.is_empty() {
            info!("Session {} disconnected, left channels {:?}", session_id, result);
        }
        result
    }

    /// 获取频道在线 session 列表
    pub fn get_channel_sessions(&self, channel_id: u64) -> Vec<SessionId> {
        self.channel_routes
            .get(&channel_id)
            .map(|sessions| sessions.clone())
            .unwrap_or_default()
    }

    /// 获取 session 当前订阅的所有频道
    pub fn get_session_channels(&self, session_id: &SessionId) -> HashSet<u64> {
        self.session_channels
            .get(session_id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// 获取频道在线人数
    pub fn get_channel_online_count(&self, channel_id: u64) -> usize {
        self.channel_routes.get(&channel_id).map(|s| s.len()).unwrap_or(0)
    }

    /// 获取所有频道数
    pub fn get_channel_count(&self) -> usize {
        self.channel_routes.len()
    }

    /// 获取总 session 数
    pub fn get_total_session_count(&self) -> usize {
        self.session_channels.len()
    }

    /// 获取所有频道列表（channel_id, session_count）
    pub fn get_all_channels(&self) -> Vec<(u64, usize)> {
        self.channel_routes
            .iter()
            .map(|entry| (*entry.key(), entry.value().len()))
            .collect()
    }
}

/// 频道订阅相关常量
pub const MAX_CHANNEL_SUBSCRIBERS_ONLINE: usize = 20000;
/// 每个 session 最多同时订阅的频道数（防止 UI 泄漏导致内存增长）
pub const MAX_SUBSCRIPTIONS_PER_SESSION: usize = 32;

impl Default for SubscribeManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(id: u64) -> SessionId {
        SessionId::from(id)
    }

    #[test]
    fn test_subscribe_unsubscribe() {
        let manager = SubscribeManager::new();

        // 订阅频道1
        manager.subscribe(sid(1), 100).unwrap();
        assert_eq!(manager.get_channel_sessions(100).len(), 1);
        assert!(manager.get_session_channels(&sid(1)).contains(&100));

        // 同时订阅频道2（不会踢掉频道1）
        manager.subscribe(sid(1), 200).unwrap();
        assert_eq!(manager.get_channel_sessions(100).len(), 1);
        assert_eq!(manager.get_channel_sessions(200).len(), 1);
        assert!(manager.get_session_channels(&sid(1)).contains(&100));
        assert!(manager.get_session_channels(&sid(1)).contains(&200));

        // 取消订阅频道1
        let removed = manager.unsubscribe(&sid(1), 100);
        assert!(removed);
        assert_eq!(manager.get_channel_sessions(100).len(), 0);
        assert_eq!(manager.get_channel_sessions(200).len(), 1);

        // 取消订阅频道2
        let removed = manager.unsubscribe(&sid(1), 200);
        assert!(removed);
        assert!(manager.get_session_channels(&sid(1)).is_empty());
    }

    #[test]
    fn test_disconnect() {
        let manager = SubscribeManager::new();

        // 订阅多个频道
        manager.subscribe(sid(1), 100).unwrap();
        manager.subscribe(sid(1), 200).unwrap();
        manager.subscribe(sid(1), 300).unwrap();
        assert_eq!(manager.get_total_session_count(), 1);

        // 断开连接应清理所有订阅
        let left = manager.on_session_disconnect(&sid(1));
        assert_eq!(left.len(), 3);
        assert_eq!(manager.get_total_session_count(), 0);
        assert_eq!(manager.get_channel_sessions(100).len(), 0);
        assert_eq!(manager.get_channel_sessions(200).len(), 0);
        assert_eq!(manager.get_channel_sessions(300).len(), 0);
    }

    #[test]
    fn test_duplicate_subscribe() {
        let manager = SubscribeManager::new();

        // 重复订阅同一频道应幂等
        manager.subscribe(sid(1), 100).unwrap();
        manager.subscribe(sid(1), 100).unwrap();
        assert_eq!(manager.get_channel_sessions(100).len(), 1);
    }

    #[test]
    fn test_max_subscriptions_per_session() {
        let manager = SubscribeManager::new();

        // 订阅到上限
        for i in 0..MAX_SUBSCRIPTIONS_PER_SESSION {
            let result = manager.subscribe(sid(1), i as u64 + 100);
            assert!(result.is_ok());
        }

        // 超过上限应被拒绝
        let result = manager.subscribe(sid(1), 999);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "too many subscriptions");

        // 取消一个后应该能再订阅
        manager.unsubscribe(&sid(1), 100);
        let result = manager.subscribe(sid(1), 999);
        assert!(result.is_ok());
    }

    #[test]
    fn test_max_channel_subscribers() {
        let manager = SubscribeManager::new();

        // 填满频道到限制
        for i in 0..MAX_CHANNEL_SUBSCRIBERS_ONLINE {
            let result = manager.subscribe(sid(i as u64 + 1000), 100);
            assert!(result.is_ok());
        }

        // 超过限制应该被拒绝
        let result = manager.subscribe(sid(999999), 100);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "channel is full");
    }
}
