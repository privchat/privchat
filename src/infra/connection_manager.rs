use anyhow::Result;
use dashmap::DashMap;
use msgtrans::SessionId;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// 设备连接信息
#[derive(Debug, Clone)]
pub struct DeviceConnection {
    pub user_id: u64,
    pub device_id: String,
    pub session_id: SessionId,
    pub connected_at: i64,
}

/// 连接管理器
///
/// 负责跟踪活跃的 WebSocket/TCP 连接，并提供设备断连功能。
/// 使用 DashMap（分片锁）替代 RwLock<HashMap>，减少锁争用。
pub struct ConnectionManager {
    /// 连接映射：(user_id, device_id) -> DeviceConnection
    connections: DashMap<(u64, String), DeviceConnection>,

    /// 反向映射：session_id -> (user_id, device_id)
    session_index: DashMap<SessionId, (u64, String)>,

    /// TransportServer 引用（用于实际断开连接）
    transport_server: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
}

impl ConnectionManager {
    /// 创建新的连接管理器
    pub fn new() -> Self {
        Self {
            connections: DashMap::new(),
            session_index: DashMap::new(),
            transport_server: Arc::new(RwLock::new(None)),
        }
    }

    /// 设置 TransportServer（在服务器启动后调用）
    pub async fn set_transport_server(&self, server: Arc<msgtrans::transport::TransportServer>) {
        let mut transport = self.transport_server.write().await;
        *transport = Some(server);
        info!("✅ ConnectionManager: TransportServer 已设置");
    }

    /// 注册设备连接
    pub async fn register_connection(
        &self,
        user_id: u64,
        device_id: String,
        session_id: SessionId,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp_millis();

        let connection = DeviceConnection {
            user_id,
            device_id: device_id.clone(),
            session_id,
            connected_at: now,
        };

        // 插入主映射
        self.connections
            .insert((user_id, device_id.clone()), connection);

        // 插入反向映射
        self.session_index
            .insert(session_id, (user_id, device_id.clone()));

        let count = self.connections.len();
        crate::infra::metrics::record_connection_count(count as u64);

        debug!(
            "📝 ConnectionManager: 注册连接 user={}, device={}, session={}",
            user_id, device_id, session_id
        );

        Ok(())
    }

    /// 注销设备连接
    pub async fn unregister_connection(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(u64, String)>> {
        // 从反向映射中移除并获取 (user_id, device_id)
        let removed = self.session_index.remove(&session_id).map(|(_, v)| v);

        if let Some((user_id, ref device_id)) = removed {
            // 从主映射中移除
            self.connections.remove(&(user_id, device_id.clone()));

            let count = self.connections.len();
            crate::infra::metrics::record_connection_count(count as u64);

            debug!(
                "📝 ConnectionManager: 注销连接 user={}, device={}, session={}",
                user_id, device_id, session_id
            );
        }

        Ok(removed)
    }

    /// 断开指定设备的连接
    ///
    /// 用于 "踢设备" 功能：立即断开指定设备的 WebSocket 连接
    pub async fn disconnect_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        // 1. 查找该设备的连接
        let connection = self
            .connections
            .get(&(user_id, device_id.to_string()))
            .map(|entry| entry.clone());

        if let Some(conn) = connection {
            info!(
                "🔌 ConnectionManager: 断开设备连接 user={}, device={}, session={}",
                user_id, device_id, conn.session_id
            );

            // 2. 获取 TransportServer
            let transport = self.transport_server.read().await;
            if let Some(server) = transport.as_ref() {
                // 3. 断开连接
                if let Err(e) = server.close_session(conn.session_id).await {
                    warn!(
                        "⚠️ ConnectionManager: 关闭会话失败 session={}, error={}",
                        conn.session_id, e
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

            // 4. 清理连接映射
            let _ = self.unregister_connection(conn.session_id).await?;
        } else {
            debug!(
                "📝 ConnectionManager: 设备未连接 user={}, device={}",
                user_id, device_id
            );
        }

        Ok(())
    }

    /// 断开用户的所有其他设备（保留当前设备）
    pub async fn disconnect_other_devices(
        &self,
        user_id: u64,
        current_device_id: &str,
    ) -> Result<Vec<String>> {
        // 1. 收集该用户的其他设备 ID（只持有引用，不持有锁）
        let devices_to_disconnect: Vec<String> = self
            .connections
            .iter()
            .filter(|entry| {
                let (uid, device_id) = entry.key();
                *uid == user_id && device_id != current_device_id
            })
            .map(|entry| entry.key().1.clone())
            .collect();

        info!(
            "🔌 ConnectionManager: 断开其他设备 user={}, count={}, current_device={}",
            user_id,
            devices_to_disconnect.len(),
            current_device_id
        );

        // 2. 逐个断开
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
        self.connections
            .iter()
            .filter(|entry| entry.key().0 == user_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 获取活跃连接数
    pub async fn get_connection_count(&self) -> usize {
        self.connections.len()
    }

    /// 检查设备是否在线
    pub async fn is_device_online(&self, user_id: u64, device_id: &str) -> bool {
        self.connections
            .contains_key(&(user_id, device_id.to_string()))
    }

    /// 实时推送一条 PushMessageRequest 到用户当前所有在线连接
    pub async fn send_push_to_user(
        &self,
        user_id: u64,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        let sessions: Vec<SessionId> = self
            .get_user_connections(user_id)
            .await
            .into_iter()
            .map(|c| c.session_id)
            .collect();

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
            let mut packet = msgtrans::packet::Packet::one_way(0u32, payload.clone());
            packet.set_biz_type(privchat_protocol::protocol::MessageType::PushMessageRequest as u8);
            match server.send_to_session(sid, packet).await {
                Ok(_) => success += 1,
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

        // 注册连接
        manager
            .register_connection(1, "device-001".to_string(), session_id)
            .await
            .unwrap();

        // 检查是否在线
        assert!(manager.is_device_online(1, "device-001").await);

        // 检查连接数
        assert_eq!(manager.get_connection_count().await, 1);

        // 注销连接
        manager.unregister_connection(session_id).await.unwrap();

        // 检查是否离线
        assert!(!manager.is_device_online(1, "device-001").await);
        assert_eq!(manager.get_connection_count().await, 0);
    }

    #[tokio::test]
    async fn test_multiple_devices() {
        let manager = ConnectionManager::new();

        // 注册多个设备
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

        // 检查连接数
        assert_eq!(manager.get_connection_count().await, 3);

        // 获取用户连接
        let connections = manager.get_user_connections(1).await;
        assert_eq!(connections.len(), 3);
    }
}
