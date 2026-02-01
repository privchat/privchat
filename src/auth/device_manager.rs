use crate::auth::models::{Device, DeviceItem};
use crate::error::{Result, ServerError};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// 设备管理服务
pub struct DeviceManager {
    /// 存储：user_id -> Vec<Device>
    devices: Arc<RwLock<HashMap<u64, Vec<Device>>>>,
    
    /// 存储：device_id -> Device (快速查找)
    device_index: Arc<RwLock<HashMap<String, Device>>>,
}

impl DeviceManager {
    /// 创建新的设备管理器
    pub fn new() -> Self {
        Self {
            devices: Arc::new(RwLock::new(HashMap::new())),
            device_index: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 注册新设备
    pub async fn register_device(&self, device: Device) -> Result<()> {
        let user_id = device.user_id;
        let device_id = device.device_id.clone();
        
        debug!("注册设备: user_id={}, device_id={}", user_id, device_id);
        
        // 1. 添加到用户设备列表
        let mut devices = self.devices.write().await;
        devices
            .entry(user_id)
            .or_insert_with(Vec::new)
            .push(device.clone());
        
        // 2. 添加到设备索引
        drop(devices); // 释放锁
        let mut index = self.device_index.write().await;
        index.insert(device_id.clone(), device);
        
        info!("✅ 设备注册成功: device_id={}", device_id);
        
        Ok(())
    }
    
    /// 获取用户的所有设备
    pub async fn get_user_devices(&self, user_id: u64) -> Vec<Device> {
        let devices = self.devices.read().await;
        devices.get(&user_id).cloned().unwrap_or_default()
    }
    
    /// 获取用户的所有设备（包含当前设备标记）
    pub async fn get_user_devices_with_current(
        &self,
        user_id: u64,
        current_device_id: Option<&str>,
    ) -> Vec<DeviceItem> {
        let devices = self.get_user_devices(user_id).await;
        
        devices
            .into_iter()
            .map(|device| {
                let is_current = current_device_id
                    .map(|id| id == device.device_id)
                    .unwrap_or(false);
                
                DeviceItem {
                    device_id: device.device_id,
                    device_name: device.device_info.device_name,
                    device_model: device.device_info.device_model,
                    app_id: device.device_info.app_id,
                    device_type: device.device_type,
                    last_active_at: device.last_active_at,
                    created_at: device.created_at,
                    ip_address: device.ip_address,
                    is_current,
                }
            })
            .collect()
    }
    
    /// 根据设备ID获取设备
    pub async fn get_device(&self, device_id: &str) -> Option<Device> {
        let index = self.device_index.read().await;
        index.get(device_id).cloned()
    }
    
    /// 更新设备最后活跃时间
    pub async fn update_last_active(&self, device_id: &str) -> Result<()> {
        let mut index = self.device_index.write().await;
        
        if let Some(device) = index.get_mut(device_id) {
            device.last_active_at = Utc::now();
            let user_id = device.user_id.clone();
            drop(index);
            
            // 同时更新用户设备列表中的设备
            let mut devices = self.devices.write().await;
            if let Some(user_devices) = devices.get_mut(&user_id) {
                if let Some(dev) = user_devices
                    .iter_mut()
                    .find(|d| d.device_id == device_id)
                {
                    dev.last_active_at = Utc::now();
                }
            }
            
            debug!("更新设备活跃时间: device_id={}", device_id);
            Ok(())
        } else {
            warn!("设备不存在: device_id={}", device_id);
            Err(ServerError::NotFound(format!(
                "设备不存在: {}",
                device_id
            )))
        }
    }
    
    /// 删除设备
    pub async fn remove_device(&self, device_id: &str) -> Result<Device> {
        debug!("删除设备: device_id={}", device_id);
        
        // 1. 从索引中删除
        let mut index = self.device_index.write().await;
        let device = index
            .remove(device_id)
            .ok_or_else(|| ServerError::NotFound(format!("设备不存在: {}", device_id)))?;
        
        let user_id = device.user_id;
        drop(index);
        
        // 2. 从用户设备列表中删除
        let mut devices = self.devices.write().await;
        if let Some(user_devices) = devices.get_mut(&user_id) {
            user_devices.retain(|d| d.device_id != device_id);
            
            // 如果用户没有设备了，删除用户条目
            if user_devices.is_empty() {
                devices.remove(&user_id);
            }
        }
        
        info!("✅ 设备已删除: device_id={}", device_id);
        
        Ok(device)
    }
    
    /// 删除用户的所有设备
    pub async fn remove_all_user_devices(&self, user_id: u64) -> Result<Vec<Device>> {
        debug!("删除用户所有设备: user_id={}", user_id);
        
        let devices = self.get_user_devices(user_id).await;
        let count = devices.len();
        
        if devices.is_empty() {
            return Ok(vec![]);
        }
        
        // 从索引中删除所有设备
        let mut index = self.device_index.write().await;
        for device in &devices {
            index.remove(&device.device_id);
        }
        drop(index);
        
        // 从用户设备列表中删除
        let mut user_devices = self.devices.write().await;
        user_devices.remove(&user_id);
        
        info!("✅ 已删除用户 {} 的所有 {} 个设备", user_id, count);
        
        Ok(devices)
    }
    
    /// 更新设备名称
    pub async fn update_device_name(
        &self,
        device_id: &str,
        new_name: String,
    ) -> Result<()> {
        debug!(
            "更新设备名称: device_id={}, new_name={}",
            device_id, new_name
        );
        
        let mut index = self.device_index.write().await;
        
        if let Some(device) = index.get_mut(device_id) {
            device.device_info.device_name = new_name.clone();
            let user_id = device.user_id;
            drop(index);
            
            // 同步到用户设备列表
            let mut devices = self.devices.write().await;
            if let Some(user_devices) = devices.get_mut(&user_id) {
                if let Some(dev) = user_devices
                    .iter_mut()
                    .find(|d| d.device_id == device_id)
                {
                    dev.device_info.device_name = new_name;
                }
            }
            
            info!("✅ 设备名称已更新: device_id={}", device_id);
            Ok(())
        } else {
            warn!("设备不存在: device_id={}", device_id);
            Err(ServerError::NotFound(format!(
                "设备不存在: {}",
                device_id
            )))
        }
    }
    
    /// 获取设备统计信息
    pub async fn get_stats(&self) -> DeviceStats {
        let devices = self.devices.read().await;
        let index = self.device_index.read().await;
        
        DeviceStats {
            total_users: devices.len(),
            total_devices: index.len(),
        }
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 设备统计信息
#[derive(Debug, Clone)]
pub struct DeviceStats {
    pub total_users: usize,
    pub total_devices: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::models::{DeviceInfo, DeviceType};
    
    fn create_test_device(user_id: u64, device_id: &str) -> Device {
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
            token_jti: "test-jti".to_string(),
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
    async fn test_register_and_get_device() {
        let manager = DeviceManager::new();
        
        let device = create_test_device("alice", "device-1");
        manager.register_device(device.clone()).await.unwrap();
        
        let retrieved = manager.get_device("device-1").await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().user_id, "alice");
    }
    
    #[tokio::test]
    async fn test_get_user_devices() {
        let manager = DeviceManager::new();
        
        manager
            .register_device(create_test_device("alice", "device-1"))
            .await
            .unwrap();
        manager
            .register_device(create_test_device("alice", "device-2"))
            .await
            .unwrap();
        
        let devices = manager.get_user_devices("alice").await;
        assert_eq!(devices.len(), 2);
    }
    
    #[tokio::test]
    async fn test_remove_device() {
        let manager = DeviceManager::new();
        
        manager
            .register_device(create_test_device("alice", "device-1"))
            .await
            .unwrap();
        
        let removed = manager.remove_device("device-1").await.unwrap();
        assert_eq!(removed.device_id, "device-1");
        
        let retrieved = manager.get_device("device-1").await;
        assert!(retrieved.is_none());
    }
    
    #[tokio::test]
    async fn test_update_device_name() {
        let manager = DeviceManager::new();
        
        manager
            .register_device(create_test_device("alice", "device-1"))
            .await
            .unwrap();
        
        manager
            .update_device_name("device-1", "My iPhone".to_string())
            .await
            .unwrap();
        
        let device = manager.get_device("device-1").await.unwrap();
        assert_eq!(device.device_info.device_name, "My iPhone");
    }
}

