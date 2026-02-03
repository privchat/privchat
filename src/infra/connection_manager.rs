use anyhow::Result;
use msgtrans::SessionId;
use std::collections::HashMap;
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
/// 负责跟踪活跃的 WebSocket/TCP 连接，并提供设备断连功能
pub struct ConnectionManager {
    /// 连接映射：(user_id, device_id) -> DeviceConnection
    connections: Arc<RwLock<HashMap<(u64, String), DeviceConnection>>>,
    
    /// 反向映射：session_id -> (user_id, device_id)
    session_index: Arc<RwLock<HashMap<SessionId, (u64, String)>>>,
    
    /// TransportServer 引用（用于实际断开连接）
    transport_server: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
}

impl ConnectionManager {
    /// 创建新的连接管理器
    pub fn new() -> Self {
        Self {
            connections: Arc::new(RwLock::new(HashMap::new())),
            session_index: Arc::new(RwLock::new(HashMap::new())),
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

        // 更新主映射（完成后释放锁，再取 session_index 避免死锁）
        let count = {
            let mut connections = self.connections.write().await;
            connections.insert((user_id, device_id.clone()), connection);
            connections.len()
        };

        // 更新反向映射
        let mut session_index = self.session_index.write().await;
        session_index.insert(session_id, (user_id, device_id.clone()));

        crate::infra::metrics::record_connection_count(count as u64);

        debug!(
            "📝 ConnectionManager: 注册连接 user={}, device={}, session={}",
            user_id, device_id, session_id
        );

        Ok(())
    }

    /// 注销设备连接
    pub async fn unregister_connection(&self, session_id: SessionId) -> Result<()> {
        // 从反向映射中获取 user_id 和 device_id，然后释放锁避免与 connections 死锁
        let removed = {
            let mut session_index = self.session_index.write().await;
            session_index.remove(&session_id)
        };
        if let Some((user_id, device_id)) = removed {
            let mut connections = self.connections.write().await;
            connections.remove(&(user_id, device_id.clone()));
            let count = connections.len();
            drop(connections);
            crate::infra::metrics::record_connection_count(count as u64);

            debug!(
                "📝 ConnectionManager: 注销连接 user={}, device={}, session={}",
                user_id, device_id, session_id
            );
        }

        Ok(())
    }

    /// 断开指定设备的连接
    /// 
    /// 用于 "踢设备" 功能：立即断开指定设备的 WebSocket 连接
    pub async fn disconnect_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        // 1. 查找该设备的连接
        let connections = self.connections.read().await;
        let connection = connections
            .get(&(user_id, device_id.to_string()))
            .cloned();
        
        drop(connections); // 释放读锁

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
                warn!(
                    "⚠️ ConnectionManager: TransportServer 未设置，无法断开连接"
                );
            }

            // 4. 清理连接映射
            self.unregister_connection(conn.session_id).await?;
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
        // 1. 查找该用户的所有连接
        let connections = self.connections.read().await;
        let devices_to_disconnect: Vec<String> = connections
            .iter()
            .filter(|((uid, device_id), _)| {
                *uid == user_id && device_id != current_device_id
            })
            .map(|((_, device_id), _)| device_id.clone())
            .collect();
        
        drop(connections); // 释放读锁

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
        let connections = self.connections.read().await;
        connections
            .iter()
            .filter(|((uid, _), _)| *uid == user_id)
            .map(|(_, conn)| conn.clone())
            .collect()
    }

    /// 获取活跃连接数
    pub async fn get_connection_count(&self) -> usize {
        let connections = self.connections.read().await;
        connections.len()
    }

    /// 检查设备是否在线
    pub async fn is_device_online(&self, user_id: u64, device_id: &str) -> bool {
        let connections = self.connections.read().await;
        connections.contains_key(&(user_id, device_id.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_register_and_unregister() {
        let manager = ConnectionManager::new();
        let session_id = SessionId(123);
        
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
            .register_connection(1, "device-001".to_string(), SessionId(101))
            .await
            .unwrap();
        manager
            .register_connection(1, "device-002".to_string(), SessionId(102))
            .await
            .unwrap();
        manager
            .register_connection(1, "device-003".to_string(), SessionId(103))
            .await
            .unwrap();
        
        // 检查连接数
        assert_eq!(manager.get_connection_count().await, 3);
        
        // 获取用户连接
        let connections = manager.get_user_connections(1).await;
        assert_eq!(connections.len(), 3);
    }
}
