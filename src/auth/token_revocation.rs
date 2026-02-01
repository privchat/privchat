use crate::auth::device_manager::DeviceManager;
use crate::error::{Result, ServerError};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Token 撤销服务
pub struct TokenRevocationService {
    /// Token 黑名单 (存储 jti)
    blacklist: Arc<RwLock<HashSet<String>>>,
    
    /// 设备管理器
    device_manager: Arc<DeviceManager>,
}

impl TokenRevocationService {
    /// 创建新的撤销服务
    pub fn new(device_manager: Arc<DeviceManager>) -> Self {
        Self {
            blacklist: Arc::new(RwLock::new(HashSet::new())),
            device_manager,
        }
    }
    
    /// 撤销指定设备的 token
    pub async fn revoke_device(&self, device_id: &str) -> Result<()> {
        debug!("撤销设备 token: device_id={}", device_id);
        
        // 1. 获取设备信息
        let device = self
            .device_manager
            .get_device(device_id)
            .await
            .ok_or_else(|| {
                ServerError::NotFound(format!("设备不存在: {}", device_id))
            })?;
        
        // 2. 将 token 的 jti 加入黑名单
        let mut blacklist = self.blacklist.write().await;
        blacklist.insert(device.token_jti.clone());
        drop(blacklist);
        
        // 3. 删除设备记录
        self.device_manager.remove_device(device_id).await?;
        
        info!(
            "✅ 已撤销设备 {} 的 token (jti: {})",
            device_id, device.token_jti
        );
        
        Ok(())
    }
    
    /// 撤销用户的所有设备 token
    pub async fn revoke_all_devices(&self, user_id: u64) -> Result<usize> {
        debug!("撤销用户所有设备 token: user_id={}", user_id);
        
        let devices = self.device_manager.get_user_devices(user_id).await;
        
        if devices.is_empty() {
            warn!("用户没有设备: user_id={}", user_id);
            return Ok(0);
        }
        
        let count = devices.len();
        let mut blacklist = self.blacklist.write().await;
        
        // 将所有 token 加入黑名单
        for device in &devices {
            blacklist.insert(device.token_jti.clone());
        }
        drop(blacklist);
        
        // 删除所有设备
        self.device_manager
            .remove_all_user_devices(user_id)
            .await?;
        
        info!("✅ 已撤销用户 {} 的所有 {} 个设备 token", user_id, count);
        
        Ok(count)
    }
    
    /// 检查 token 是否被撤销
    pub async fn is_revoked(&self, jti: &str) -> bool {
        let blacklist = self.blacklist.read().await;
        blacklist.contains(jti)
    }
    
    /// 清理过期的黑名单条目（可定期调用）
    /// 注意：这个方法需要 token 的过期时间信息，暂时简化实现
    pub async fn cleanup_expired(&self) -> usize {
        // TODO: 实现基于过期时间的清理逻辑
        // 当前简化实现：返回 0
        0
    }
    
    /// 获取黑名单大小
    pub async fn blacklist_size(&self) -> usize {
        let blacklist = self.blacklist.read().await;
        blacklist.len()
    }
    
    /// 清空黑名单（仅用于测试）
    #[cfg(test)]
    pub async fn clear_blacklist(&self) {
        let mut blacklist = self.blacklist.write().await;
        blacklist.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::models::{Device, DeviceInfo, DeviceType};
    use chrono::Utc;
    
    fn create_test_device(user_id: u64, device_id: &str, jti: &str) -> Device {
        Device {
            device_id: device_id.to_string(),
            user_id: user_id,
            business_system_id: "test".to_string(),
            device_info: DeviceInfo {
                app_id: "ios".to_string(),
                device_name: "Test Device".to_string(),
                device_model: "iPhone".to_string(),
                os_version: "iOS 17".to_string(),
                app_version: "1.0.0".to_string(),
            },
            device_type: DeviceType::IOS,
            token_jti: jti.to_string(),
            session_version: 1,
            session_state: crate::auth::SessionState::Active,
            kicked_at: None,
            kicked_reason: None,
            last_active_at: Utc::now(),
            created_at: Utc::now(),
            ip_address: "127.0.0.1".to_string(),
        }
    }
    
    #[tokio::test]
    async fn test_revoke_device() {
        let device_manager = Arc::new(DeviceManager::new());
        let service = TokenRevocationService::new(device_manager.clone());
        
        let device = create_test_device("alice", "device-1", "jti-123");
        device_manager.register_device(device).await.unwrap();
        
        // 撤销设备
        service.revoke_device("device-1").await.unwrap();
        
        // 检查 jti 是否在黑名单中
        assert!(service.is_revoked("jti-123").await);
        
        // 检查设备是否被删除
        assert!(device_manager.get_device("device-1").await.is_none());
    }
    
    #[tokio::test]
    async fn test_revoke_all_devices() {
        let device_manager = Arc::new(DeviceManager::new());
        let service = TokenRevocationService::new(device_manager.clone());
        
        device_manager
            .register_device(create_test_device("alice", "device-1", "jti-1"))
            .await
            .unwrap();
        device_manager
            .register_device(create_test_device("alice", "device-2", "jti-2"))
            .await
            .unwrap();
        
        // 撤销所有设备
        let count = service.revoke_all_devices("alice").await.unwrap();
        assert_eq!(count, 2);
        
        // 检查所有 jti 是否在黑名单中
        assert!(service.is_revoked("jti-1").await);
        assert!(service.is_revoked("jti-2").await);
        
        // 检查所有设备是否被删除
        assert_eq!(device_manager.get_user_devices("alice").await.len(), 0);
    }
    
    #[tokio::test]
    async fn test_is_not_revoked() {
        let device_manager = Arc::new(DeviceManager::new());
        let service = TokenRevocationService::new(device_manager);
        
        assert!(!service.is_revoked("non-existent-jti").await);
    }
}

