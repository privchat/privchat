use std::time::Duration;
use dashmap::DashMap;
use std::sync::Arc;
use tokio::time::interval;
use tracing::{debug, info};
use serde::{Deserialize, Serialize};
use chrono::{DateTime, Utc};

use crate::config::OnlineStatusConfig;
use crate::error::ServerError;
use privchat_protocol::DeviceType;

/// 用户在线会话
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineSession {
    pub user_id: String,
    pub session_id: String,
    pub device_type: DeviceType,
    pub device_id: String,
    pub connected_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

impl OnlineSession {
    pub fn new(
        user_id: String,
        session_id: String,
        device_type: DeviceType,
        device_id: String,
    ) -> Self {
        let now = Utc::now();
        Self {
            user_id,
            session_id,
            device_type,
            device_id,
            connected_at: now,
            last_heartbeat: now,
            ip_address: None,
            user_agent: None,
        }
    }
    
    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = Utc::now();
    }
    
    pub fn is_online(&self, timeout: Duration) -> bool {
        let elapsed = Utc::now().signed_duration_since(self.last_heartbeat);
        elapsed.to_std().unwrap_or(Duration::MAX) < timeout
    }
    
    pub fn with_ip_address(mut self, ip: String) -> Self {
        self.ip_address = Some(ip);
        self
    }
    
    pub fn with_user_agent(mut self, user_agent: String) -> Self {
        self.user_agent = Some(user_agent);
        self
    }
    
    pub fn session_key(&self) -> String {
        format!("{}:{}", self.user_id, self.device_id)
    }
}

/// 高性能在线状态管理器
/// 使用 DashMap 提供高并发读写性能
pub struct OnlineStatusManager {
    /// 在线会话映射 (user_id:device_id -> OnlineSession)
    sessions: Arc<DashMap<String, OnlineSession>>,
    /// 会话ID到会话键的映射 (session_id -> user_id:device_id)
    session_to_key: Arc<DashMap<String, String>>,
    /// 用户到设备列表的映射 (user_id -> Vec<device_id>)
    user_devices: Arc<DashMap<u64, Vec<String>>>,
    /// 配置
    config: OnlineStatusConfig,
}

impl OnlineStatusManager {
    pub fn new(config: OnlineStatusConfig) -> Self {
        let manager = Self {
            sessions: Arc::new(DashMap::new()),
            session_to_key: Arc::new(DashMap::new()),
            user_devices: Arc::new(DashMap::new()),
            config,
        };
        
        // 启动清理任务
        manager.start_cleanup_task();
        
        info!("🚀 OnlineStatusManager initialized with DashMap");
        manager
    }
    
    /// 用户设备上线
    pub fn user_online(
        &self,
        user_id: u64,
        session_id: String,
        device_type: DeviceType,
        device_id: String,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<(), ServerError> {
        let session_key = format!("{}:{}", user_id, device_id);
        
        let mut session = OnlineSession::new(user_id.to_string(), session_id.clone(), device_type, device_id.clone());
        if let Some(ip) = ip_address {
            session = session.with_ip_address(ip);
        }
        if let Some(ua) = user_agent {
            session = session.with_user_agent(ua);
        }
        
        // 更新会话映射
        self.sessions.insert(session_key.clone(), session);
        
        // 更新会话ID映射
        self.session_to_key.insert(session_id, session_key);
        
        // 更新用户设备列表
        self.user_devices.entry(user_id)
            .and_modify(|devices| {
                if !devices.contains(&device_id) {
                    devices.push(device_id.clone());
                }
            })
            .or_insert_with(|| vec![device_id.clone()]);
        
        debug!("👤 User {}:{} is now online", user_id, device_id);
        Ok(())
    }
    
    /// 用户设备下线
    pub fn user_offline(&self, user_id: u64, device_id: &str) -> Result<(), ServerError> {
        let session_key = format!("{}:{}", user_id, device_id);
        
        // 移除会话
        if let Some((_, session)) = self.sessions.remove(&session_key) {
            // 移除会话ID映射
            self.session_to_key.remove(&session.session_id);
            
            // 更新用户设备列表
            self.user_devices.entry(user_id)
                .and_modify(|devices| {
                    devices.retain(|d| d != device_id);
                });
            
            // 如果用户没有在线设备，移除用户记录
            if let Some(devices) = self.user_devices.get(&user_id) {
                if devices.is_empty() {
                    self.user_devices.remove(&user_id);
                }
            }
            
            debug!("👤 User {}:{} is now offline", user_id, device_id);
        }
        
        Ok(())
    }
    
    /// 通过会话ID下线用户
    pub fn user_offline_by_session(&self, session_id: &str) -> Result<(), ServerError> {
        if let Some((_, session_key)) = self.session_to_key.remove(session_id) {
            if let Some((_, _session)) = self.sessions.remove(&session_key) {
                // 解析用户ID和设备ID
                let parts: Vec<&str> = session_key.split(':').collect();
                if parts.len() == 2 {
                    let user_id = parts[0].parse::<u64>().unwrap_or(0);
                    let device_id = parts[1];
                    
                    // 更新用户设备列表
                    self.user_devices.entry(user_id)
                        .and_modify(|devices| {
                            devices.retain(|d| d != device_id);
                        });
                    
                    // 如果用户没有在线设备，移除用户记录
                    if let Some(devices) = self.user_devices.get(&user_id) {
                        if devices.is_empty() {
                            self.user_devices.remove(&user_id);
                        }
                    }
                    
                    debug!("👤 User {}:{} offline by session", user_id, device_id);
                }
            }
        }
        
        Ok(())
    }
    
    /// 更新用户心跳
    pub fn update_heartbeat(&self, user_id: u64, device_id: &str) -> Result<(), ServerError> {
        let session_key = format!("{}:{}", user_id, device_id);
        
        if let Some(mut session) = self.sessions.get_mut(&session_key) {
            session.update_heartbeat();
            debug!("💓 Updated heartbeat for {}:{}", user_id, device_id);
        }
        
        Ok(())
    }
    
    /// 通过会话ID更新心跳
    pub fn update_heartbeat_by_session(&self, session_id: &str) -> Result<(), ServerError> {
        if let Some(session_key) = self.session_to_key.get(session_id) {
            if let Some(mut session) = self.sessions.get_mut(session_key.as_str()) {
                session.update_heartbeat();
                debug!("💓 Updated heartbeat for session {}", session_id);
            }
        }
        
        Ok(())
    }
    
    /// 检查用户设备是否在线
    pub fn is_device_online(&self, user_id: u64, device_id: &str) -> bool {
        let session_key = format!("{}:{}", user_id, device_id);
        
        if let Some(session) = self.sessions.get(&session_key) {
            session.is_online(Duration::from_secs(self.config.offline_timeout_secs))
        } else {
            false
        }
    }
    
    /// 检查用户是否在线（任意设备）
    pub fn is_user_online(&self, user_id: u64) -> bool {
        if let Some(devices) = self.user_devices.get(&user_id) {
            let timeout = Duration::from_secs(self.config.offline_timeout_secs);
            
            for device_id in devices.iter() {
                let session_key = format!("{}:{}", user_id, device_id);
                if let Some(session) = self.sessions.get(&session_key) {
                    if session.is_online(timeout) {
                        return true;
                    }
                }
            }
        }
        
        false
    }
    
    /// 获取用户的在线设备列表
    pub fn get_user_online_devices(&self, user_id: u64) -> Vec<String> {
        let mut online_devices = Vec::new();
        
        if let Some(devices) = self.user_devices.get(&user_id) {
            let timeout = Duration::from_secs(self.config.offline_timeout_secs);
            
            for device_id in devices.iter() {
                let session_key = format!("{}:{}", user_id, device_id);
                if let Some(session) = self.sessions.get(&session_key) {
                    if session.is_online(timeout) {
                        online_devices.push(device_id.clone());
                    }
                }
            }
        }
        
        online_devices
    }
    
    /// 获取用户的所有在线会话
    pub fn get_user_sessions(&self, user_id: u64) -> Vec<OnlineSession> {
        let mut sessions = Vec::new();
        
        if let Some(devices) = self.user_devices.get(&user_id) {
            let timeout = Duration::from_secs(self.config.offline_timeout_secs);
            
            for device_id in devices.iter() {
                let session_key = format!("{}:{}", user_id, device_id);
                if let Some(session) = self.sessions.get(&session_key) {
                    if session.is_online(timeout) {
                        sessions.push(session.clone());
                    }
                }
            }
        }
        
        sessions
    }
    
    /// 获取所有在线用户
    pub fn get_online_users(&self) -> Vec<u64> {
        let timeout = Duration::from_secs(self.config.offline_timeout_secs);
        let mut online_users = Vec::new();
        
        for user_entry in self.user_devices.iter() {
            let user_id = user_entry.key();
            let devices = user_entry.value();
            
            let mut has_online_device = false;
            for device_id in devices.iter() {
                let session_key = format!("{}:{}", user_id, device_id);
                if let Some(session) = self.sessions.get(&session_key) {
                    if session.is_online(timeout) {
                        has_online_device = true;
                        break;
                    }
                }
            }
            
            if has_online_device {
                online_users.push(*user_id);
            }
        }
        
        online_users
    }
    
    /// 获取在线用户数量
    pub fn get_online_user_count(&self) -> usize {
        self.get_online_users().len()
    }
    
    /// 获取在线会话数量
    pub fn get_online_session_count(&self) -> usize {
        let timeout = Duration::from_secs(self.config.offline_timeout_secs);
        
        self.sessions.iter()
            .filter(|entry| entry.value().is_online(timeout))
            .count()
    }
    
    /// 获取总会话数量（包括离线）
    pub fn get_total_session_count(&self) -> usize {
        self.sessions.len()
    }

    /// 简化的用户上线方法，兼容服务器调用
    pub fn simple_user_online(
        &self,
        session_id: String,
        user_id: String,
        device_id: String,
        device_type: DeviceType,
        ip_address: String,
    ) {
        let user_id_u64 = user_id.parse::<u64>().unwrap_or(0);
        let _ = self.user_online(
            user_id_u64,
            session_id,
            device_type,
            device_id,
            Some(ip_address),
            None,
        );
    }

    /// 简化的用户下线方法，兼容服务器调用
    pub fn simple_user_offline(&self, session_id: &str) -> bool {
        self.user_offline_by_session(session_id).is_ok()
    }

    /// 简化的心跳更新方法，兼容服务器调用
    pub fn simple_update_heartbeat(&self, session_id: &str) -> bool {
        self.update_heartbeat_by_session(session_id).is_ok()
    }
    
    /// 按设备类型统计在线用户
    pub fn get_online_stats_by_device(&self) -> std::collections::HashMap<DeviceType, usize> {
        let timeout = Duration::from_secs(self.config.offline_timeout_secs);
        let mut stats = std::collections::HashMap::new();
        
        for entry in self.sessions.iter() {
            let session = entry.value();
            if session.is_online(timeout) {
                *stats.entry(session.device_type.clone()).or_insert(0) += 1;
            }
        }
        
        stats
    }
    
    /// 清理过期的会话
    pub fn cleanup_expired_sessions(&self) -> usize {
        let timeout = Duration::from_secs(self.config.offline_timeout_secs);
        let mut expired_keys = Vec::new();
        let mut expired_session_ids = Vec::new();
        
        // 找出过期的会话
        for entry in self.sessions.iter() {
            let session = entry.value();
            if !session.is_online(timeout) {
                expired_keys.push(entry.key().clone());
                expired_session_ids.push(session.session_id.clone());
            }
        }
        
        let expired_count = expired_keys.len();
        
        // 移除过期的会话
        for key in expired_keys {
            self.sessions.remove(&key);
        }
        
        // 移除过期的会话ID映射
        for session_id in expired_session_ids {
            self.session_to_key.remove(&session_id);
        }
        
        // 清理空的用户设备列表
        let mut empty_users = Vec::new();
        for user_entry in self.user_devices.iter() {
            let user_id = user_entry.key();
            let devices = user_entry.value();
            
            let mut has_online_device = false;
            for device_id in devices.iter() {
                let session_key = format!("{}:{}", user_id, device_id);
                if self.sessions.contains_key(&session_key) {
                    has_online_device = true;
                    break;
                }
            }
            
            if !has_online_device {
                empty_users.push(user_id.clone());
            }
        }
        
        for user_id in empty_users {
            self.user_devices.remove(&user_id);
        }
        
        if expired_count > 0 {
            info!("🧹 Cleaned up {} expired sessions", expired_count);
        }
        
        expired_count
    }
    
    /// 启动清理任务
    fn start_cleanup_task(&self) {
        let sessions = Arc::clone(&self.sessions);
        let session_to_key = Arc::clone(&self.session_to_key);
        let user_devices = Arc::clone(&self.user_devices);
        let config = self.config.clone();
        
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(config.cleanup_interval_secs));
            
            loop {
                interval.tick().await;
                
                let timeout = Duration::from_secs(config.offline_timeout_secs);
                let mut expired_keys = Vec::new();
                let mut expired_session_ids = Vec::new();
                
                // 找出过期的会话
                for entry in sessions.iter() {
                    let session = entry.value();
                    if !session.is_online(timeout) {
                        expired_keys.push(entry.key().clone());
                        expired_session_ids.push(session.session_id.clone());
                    }
                }
                
                // 移除过期的会话
                for key in &expired_keys {
                    sessions.remove(key);
                }
                
                // 移除过期的会话ID映射
                for session_id in &expired_session_ids {
                    session_to_key.remove(session_id);
                }
                
                // 清理空的用户设备列表
                let mut empty_users = Vec::new();
                for user_entry in user_devices.iter() {
                    let user_id = user_entry.key();
                    let devices = user_entry.value();
                    
                    let mut has_online_device = false;
                    for device_id in devices.iter() {
                        let session_key = format!("{}:{}", user_id, device_id);
                        if sessions.contains_key(&session_key) {
                            has_online_device = true;
                            break;
                        }
                    }
                    
                    if !has_online_device {
                        empty_users.push(user_id.clone());
                    }
                }
                
                for user_id in empty_users {
                    user_devices.remove(&user_id);
                }
                
                if !expired_keys.is_empty() {
                    info!("🧹 Auto-cleaned up {} expired sessions", expired_keys.len());
                }
            }
        });
    }
    
    /// 获取统计信息
    pub fn get_stats(&self) -> OnlineStatusStats {
        let timeout = Duration::from_secs(self.config.offline_timeout_secs);
        
        let total_sessions = self.sessions.len();
        let online_sessions = self.sessions.iter()
            .filter(|entry| entry.value().is_online(timeout))
            .count();
        
        let total_users = self.user_devices.len();
        let online_users = self.get_online_user_count();
        
        let device_stats = self.get_online_stats_by_device();
        
        OnlineStatusStats {
            total_users,
            online_users,
            total_sessions,
            online_sessions,
            device_stats,
        }
    }
}

/// 在线状态统计
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineStatusStats {
    pub total_users: usize,
    pub online_users: usize,
    pub total_sessions: usize,
    pub online_sessions: usize,
    pub device_stats: std::collections::HashMap<DeviceType, usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::sleep;
    
    #[tokio::test]
    async fn test_online_status_manager() {
        let config = OnlineStatusConfig {
            cleanup_interval_secs: 1,
            offline_timeout_secs: 2,
            enable_persistence: false,
        };
        
        let manager = OnlineStatusManager::new(config);
        
        // 用户上线
        manager.user_online(
            "user1".to_string(),
            "session1".to_string(),
            DeviceType::iOS,
            "device1".to_string(),
            Some("192.168.1.1".to_string()),
            Some("iOS App".to_string()),
        ).unwrap();
        
        // 检查用户在线
        assert!(manager.is_user_online("user1"));
        assert!(manager.is_device_online("user1", "device1"));
        assert_eq!(manager.get_online_user_count(), 1);
        assert_eq!(manager.get_online_session_count(), 1);
        
        // 等待超时
        sleep(Duration::from_secs(3)).await;
        
        // 用户应该离线
        assert!(!manager.is_user_online("user1"));
        
        // 等待清理
        sleep(Duration::from_secs(2)).await;
        
        // 用户应该被清理
        assert_eq!(manager.get_total_session_count(), 0);
    }
    
    #[tokio::test]
    async fn test_multi_device_online() {
        let config = OnlineStatusConfig {
            cleanup_interval_secs: 60,
            offline_timeout_secs: 60,
            enable_persistence: false,
        };
        
        let manager = OnlineStatusManager::new(config);
        
        // 用户多设备上线
        manager.user_online(
            "user1".to_string(),
            "session1".to_string(),
            DeviceType::iOS,
            "device1".to_string(),
            None,
            None,
        ).unwrap();
        
        manager.user_online(
            "user1".to_string(),
            "session2".to_string(),
            DeviceType::MacOS,
            "device2".to_string(),
            None,
            None,
        ).unwrap();
        
        // 检查多设备在线
        assert!(manager.is_user_online("user1"));
        assert_eq!(manager.get_user_online_devices("user1").len(), 2);
        assert_eq!(manager.get_user_sessions("user1").len(), 2);
        assert_eq!(manager.get_online_user_count(), 1);
        assert_eq!(manager.get_online_session_count(), 2);
        
        // 一个设备下线
        manager.user_offline("user1", "device1").unwrap();
        
        // 用户仍然在线（还有一个设备）
        assert!(manager.is_user_online("user1"));
        assert_eq!(manager.get_user_online_devices("user1").len(), 1);
        assert_eq!(manager.get_online_session_count(), 1);
        
        // 最后一个设备下线
        manager.user_offline("user1", "device2").unwrap();
        
        // 用户完全离线
        assert!(!manager.is_user_online("user1"));
        assert_eq!(manager.get_online_user_count(), 0);
        assert_eq!(manager.get_online_session_count(), 0);
    }
} 