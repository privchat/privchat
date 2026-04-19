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

use anyhow::Result;
use dashmap::DashMap;
use msgtrans::SessionId;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// 设备连接信息（对外快照）
#[derive(Debug, Clone)]
pub struct DeviceConnection {
    pub user_id: u64,
    pub device_id: String,
    pub session_id: SessionId,
    pub connected_at: i64,
}

/// 用户名下单个活跃会话（内部结构）
#[derive(Debug, Clone)]
struct UserSession {
    session_id: SessionId,
    device_id: String,
    connected_at: i64,
}

impl UserSession {
    fn to_connection(&self, user_id: u64) -> DeviceConnection {
        DeviceConnection {
            user_id,
            device_id: self.device_id.clone(),
            session_id: self.session_id,
            connected_at: self.connected_at,
        }
    }
}

/// 连接管理器
///
/// 单一数据源：`sessions: user_id -> Vec<UserSession>`，以用户为主 key，
/// 推送走 O(1) 定位。`session_to_user` 只是反向路由表，服务于
/// `ConnectionClosed` 只给到 session_id 的 unregister 场景。
pub struct ConnectionManager {
    /// 主映射：user_id → 该用户当前所有活跃会话
    sessions: DashMap<u64, Vec<UserSession>>,

    /// 反向路由：session_id → user_id（仅用于 unregister 定位，不是权威源）
    session_to_user: DashMap<SessionId, u64>,

    /// 总连接数（避免遍历 DashMap 求和）
    total_connections: AtomicUsize,

    /// TransportServer 引用（用于实际断开连接）
    pub transport_server: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            session_to_user: DashMap::new(),
            total_connections: AtomicUsize::new(0),
            transport_server: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn set_transport_server(&self, server: Arc<msgtrans::transport::TransportServer>) {
        let mut transport = self.transport_server.write().await;
        *transport = Some(server);
        info!("✅ ConnectionManager: TransportServer 已设置");
    }

    /// 注册设备连接
    ///
    /// 同一 (user_id, device_id) 重连时替换旧 session：
    /// 从 Vec 中 retain 掉同 device 的旧条目，并把对应 session_id 从反向表清理，
    /// 防止旧 session 的延迟 ConnectionClosed 事件误伤新连接。
    pub async fn register_connection(
        &self,
        user_id: u64,
        device_id: String,
        session_id: SessionId,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();
        let new_session = UserSession {
            session_id,
            device_id: device_id.clone(),
            connected_at: now,
        };

        let mut replaced_session_id: Option<SessionId> = None;
        let mut added_new_slot = true;

        {
            let mut entry = self.sessions.entry(user_id).or_default();
            let list = entry.value_mut();

            if let Some(pos) = list.iter().position(|s| s.device_id == device_id) {
                let old = list.remove(pos);
                if old.session_id != session_id {
                    replaced_session_id = Some(old.session_id);
                }
                added_new_slot = false; // 替换而非新增
            }
            list.push(new_session);
        }

        if let Some(old_sid) = replaced_session_id {
            self.session_to_user.remove(&old_sid);
            debug!(
                "♻️ ConnectionManager: 同设备重连替换旧 session user={}, device={}, old={}, new={}",
                user_id, device_id, old_sid, session_id
            );
        }

        self.session_to_user.insert(session_id, user_id);

        if added_new_slot {
            let count = self.total_connections.fetch_add(1, Ordering::Relaxed) + 1;
            crate::infra::metrics::record_connection_count(count as u64);
        }

        debug!(
            "📝 ConnectionManager: 注册连接 user={}, device={}, session={}",
            user_id, device_id, session_id
        );

        Ok(())
    }

    /// 注销设备连接
    ///
    /// 仅在反向表中 session_id 仍归属该 user 且用户 Vec 中
    /// 有匹配的 session_id 时才删除，避免重连后旧 session 的延迟事件误删新连接。
    pub async fn unregister_connection(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(u64, String)>> {
        let user_id = match self.session_to_user.remove(&session_id) {
            Some((_, uid)) => uid,
            None => return Ok(None),
        };

        let mut removed: Option<(u64, String)> = None;
        let mut user_empty_after = false;

        if let Some(mut entry) = self.sessions.get_mut(&user_id) {
            let list = entry.value_mut();
            if let Some(pos) = list.iter().position(|s| s.session_id == session_id) {
                let s = list.remove(pos);
                removed = Some((user_id, s.device_id));
                self.total_connections.fetch_sub(1, Ordering::Relaxed);
            }
            user_empty_after = list.is_empty();
        }

        // 用户名下已无设备，清理主映射条目。
        // 谓词再次判空可防止读写锁之间并发 register 新 session 时误删。
        if user_empty_after {
            self.sessions.remove_if(&user_id, |_, list| list.is_empty());
        }

        if removed.is_some() {
            let count = self.total_connections.load(Ordering::Relaxed);
            crate::infra::metrics::record_connection_count(count as u64);

            debug!(
                "📝 ConnectionManager: 注销连接 user={}, session={}",
                user_id, session_id
            );
        }

        Ok(removed)
    }

    /// 断开指定设备的连接（踢设备）
    pub async fn disconnect_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        let session_id = self
            .sessions
            .get(&user_id)
            .and_then(|entry| {
                entry
                    .value()
                    .iter()
                    .find(|s| s.device_id == device_id)
                    .map(|s| s.session_id)
            });

        let Some(session_id) = session_id else {
            debug!(
                "📝 ConnectionManager: 设备未连接 user={}, device={}",
                user_id, device_id
            );
            return Ok(());
        };

        info!(
            "🔌 ConnectionManager: 断开设备连接 user={}, device={}, session={}",
            user_id, device_id, session_id
        );

        let transport = self.transport_server.read().await;
        if let Some(server) = transport.as_ref() {
            if let Err(e) = server.close_session(session_id).await {
                warn!(
                    "⚠️ ConnectionManager: 关闭会话失败 session={}, error={}",
                    session_id, e
                );
            } else {
                info!(
                    "✅ ConnectionManager: 设备连接已断开 user={}, device={}",
                    user_id, device_id
                );
            }
        } else {
            warn!("⚠️ ConnectionManager: TransportServer 未设置，无法断开连接");
        }

        let _ = self.unregister_connection(session_id).await?;
        Ok(())
    }

    /// 断开用户的所有其他设备（保留当前设备）
    pub async fn disconnect_other_devices(
        &self,
        user_id: u64,
        current_device_id: &str,
    ) -> Result<Vec<String>> {
        let devices_to_disconnect: Vec<String> = self
            .sessions
            .get(&user_id)
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .filter(|s| s.device_id != current_device_id)
                    .map(|s| s.device_id.clone())
                    .collect()
            })
            .unwrap_or_default();

        info!(
            "🔌 ConnectionManager: 断开其他设备 user={}, count={}, current_device={}",
            user_id,
            devices_to_disconnect.len(),
            current_device_id
        );

        for device_id in &devices_to_disconnect {
            if let Err(e) = self.disconnect_device(user_id, device_id).await {
                warn!(
                    "⚠️ ConnectionManager: 断开设备失败 user={}, device={}, error={}",
                    user_id, device_id, e
                );
            }
        }

        info!(
            "✅ ConnectionManager: 已断开 {} 个其他设备",
            devices_to_disconnect.len()
        );

        Ok(devices_to_disconnect)
    }

    /// 获取用户的所有活跃连接
    pub async fn get_user_connections(&self, user_id: u64) -> Vec<DeviceConnection> {
        self.sessions
            .get(&user_id)
            .map(|entry| {
                entry
                    .value()
                    .iter()
                    .map(|s| s.to_connection(user_id))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 通过 session_id 获取当前连接信息
    pub async fn get_connection_by_session(
        &self,
        session_id: &SessionId,
    ) -> Option<DeviceConnection> {
        let user_id = *self.session_to_user.get(session_id)?.value();
        self.sessions.get(&user_id).and_then(|entry| {
            entry
                .value()
                .iter()
                .find(|s| s.session_id == *session_id)
                .map(|s| s.to_connection(user_id))
        })
    }

    /// 获取活跃连接数
    pub async fn get_connection_count(&self) -> usize {
        self.total_connections.load(Ordering::Relaxed)
    }

    /// 获取所有活跃连接
    pub async fn get_all_connections(&self) -> Vec<DeviceConnection> {
        let mut out = Vec::with_capacity(self.total_connections.load(Ordering::Relaxed));
        for entry in self.sessions.iter() {
            let uid = *entry.key();
            for s in entry.value().iter() {
                out.push(s.to_connection(uid));
            }
        }
        out
    }

    /// 检查设备是否在线
    pub async fn is_device_online(&self, user_id: u64, device_id: &str) -> bool {
        self.sessions
            .get(&user_id)
            .map(|entry| entry.value().iter().any(|s| s.device_id == device_id))
            .unwrap_or(false)
    }

    /// 实时推送一条 PushMessageRequest 到用户当前所有在线连接
    ///
    /// 单次 O(1) 定位 user_id 的会话列表，不再扫描全表；
    /// 老实现 `connections.iter().filter(|e| e.key().0 == user_id)` 是 O(总连接数)，
    /// 每条消息都这么扫，用户量大时明显拖慢热路径。
    pub async fn send_push_to_user(
        &self,
        user_id: u64,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        let sessions: Vec<SessionId> = self
            .sessions
            .get(&user_id)
            .map(|entry| entry.value().iter().map(|s| s.session_id).collect())
            .unwrap_or_default();

        if sessions.is_empty() {
            return Ok(0);
        }

        let transport = self.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            warn!("⚠️ ConnectionManager: TransportServer 未设置，无法实时推送");
            return Ok(0);
        };

        let payload = privchat_protocol::encode_message(message)
            .map_err(|e| anyhow::anyhow!("encode PushMessageRequest failed: {}", e))?;

        let mut success = 0usize;
        for sid in sessions {
            let mut packet = msgtrans::packet::Packet::one_way(
                crate::infra::next_packet_id(),
                payload.clone(),
            );
            packet.set_biz_type(privchat_protocol::protocol::MessageType::PushMessageRequest as u8);
            match server.send_to_session(sid, packet).await {
                Ok(()) => success += 1,
                Err(e) => {
                    warn!(
                        "⚠️ ConnectionManager: 实时推送失败 user={}, session={}, server_message_id={}, error={}",
                        user_id, sid, message.server_message_id, e
                    );
                }
            }
        }

        Ok(success)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_unregister() {
        let manager = ConnectionManager::new();
        let session_id = SessionId::new(123);

        manager
            .register_connection(1, "device-001".to_string(), session_id)
            .await
            .unwrap();

        assert!(manager.is_device_online(1, "device-001").await);
        assert_eq!(manager.get_connection_count().await, 1);

        manager.unregister_connection(session_id).await.unwrap();

        assert!(!manager.is_device_online(1, "device-001").await);
        assert_eq!(manager.get_connection_count().await, 0);
    }

    #[tokio::test]
    async fn test_multiple_devices() {
        let manager = ConnectionManager::new();

        manager
            .register_connection(1, "device-001".to_string(), SessionId::new(101))
            .await
            .unwrap();
        manager
            .register_connection(1, "device-002".to_string(), SessionId::new(102))
            .await
            .unwrap();
        manager
            .register_connection(1, "device-003".to_string(), SessionId::new(103))
            .await
            .unwrap();

        assert_eq!(manager.get_connection_count().await, 3);
        let connections = manager.get_user_connections(1).await;
        assert_eq!(connections.len(), 3);
    }

    /// 同设备重连：旧 session 的延迟 ConnectionClosed 不应误删新连接
    #[tokio::test]
    async fn test_same_device_reconnect_does_not_kill_new_session() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(1);
        let new_sid = SessionId::new(2);

        manager
            .register_connection(1, "device-A".to_string(), old_sid)
            .await
            .unwrap();
        manager
            .register_connection(1, "device-A".to_string(), new_sid)
            .await
            .unwrap();

        // 替换而非新增：连接数依然是 1
        assert_eq!(manager.get_connection_count().await, 1);

        // 模拟旧 session 的延迟关闭事件
        let removed = manager.unregister_connection(old_sid).await.unwrap();
        assert!(removed.is_none(), "旧 session 已被替换，反向表应无记录");

        // 新连接仍在线
        assert!(manager.is_device_online(1, "device-A").await);
        assert_eq!(manager.get_connection_count().await, 1);
    }

    /// 多设备下只清自己那条
    #[tokio::test]
    async fn test_unregister_only_removes_matching_session() {
        let manager = ConnectionManager::new();
        manager
            .register_connection(1, "device-A".to_string(), SessionId::new(10))
            .await
            .unwrap();
        manager
            .register_connection(1, "device-B".to_string(), SessionId::new(20))
            .await
            .unwrap();

        manager.unregister_connection(SessionId::new(10)).await.unwrap();

        assert!(!manager.is_device_online(1, "device-A").await);
        assert!(manager.is_device_online(1, "device-B").await);
        assert_eq!(manager.get_connection_count().await, 1);
    }

    /// 用户所有设备下线后，主映射应清理该 user_id 条目
    #[tokio::test]
    async fn test_user_entry_cleaned_up_after_last_device() {
        let manager = ConnectionManager::new();
        manager
            .register_connection(42, "d".to_string(), SessionId::new(1))
            .await
            .unwrap();
        manager.unregister_connection(SessionId::new(1)).await.unwrap();

        assert_eq!(manager.get_user_connections(42).await.len(), 0);
        assert_eq!(manager.get_connection_count().await, 0);
    }
}
