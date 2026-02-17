use dashmap::DashMap;
use msgtrans::SessionId;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::error::Result;
use crate::model::user::{DeviceInfo, UserSession};

/// 会话管理器
pub struct SessionManager {
    /// 用户会话映射
    user_sessions: Arc<DashMap<u64, Arc<UserSession>>>,
    /// 会话ID到用户ID的映射
    session_to_user: Arc<DashMap<SessionId, u64>>,
}

impl SessionManager {
    /// 创建新的会话管理器
    pub fn new() -> Self {
        Self {
            user_sessions: Arc::new(DashMap::new()),
            session_to_user: Arc::new(DashMap::new()),
        }
    }

    /// 添加用户会话
    pub async fn add_user_session(
        &self,
        user_id: u64,
        session_id: SessionId,
        device_info: DeviceInfo,
    ) -> Result<()> {
        info!("Adding user session: {} -> {:?}", user_id, session_id);

        // 创建新的用户会话
        let user_session = Arc::new(UserSession::new(user_id, session_id, device_info));

        // 添加到映射中
        self.user_sessions.insert(user_id, user_session);
        self.session_to_user.insert(session_id, user_id);

        info!("User session added for user: {}", user_id);
        Ok(())
    }

    /// 获取用户会话信息
    pub async fn get_user_session_info(&self, user_id: u64) -> Option<SessionInfo> {
        if let Some(user_session) = self.user_sessions.get(&user_id) {
            Some(SessionInfo {
                session_id: user_session.session_id.to_string(),
                online_devices: vec![user_session.device_info.clone()],
            })
        } else {
            None
        }
    }

    /// 移除用户会话
    pub async fn remove_user_session(&self, user_id: u64) -> Result<()> {
        info!("Removing user session for user: {}", user_id);

        if let Some((_, user_session)) = self.user_sessions.remove(&user_id) {
            // 从会话映射中移除
            self.session_to_user.remove(&user_session.session_id);

            info!("User session removed for user: {}", user_id);
        } else {
            warn!("User session not found for user: {}", user_id);
        }

        Ok(())
    }

    /// 通过会话ID移除用户会话
    pub async fn remove_user_session_by_session_id(&self, session_id: &SessionId) -> Result<()> {
        info!("Removing user session by session ID: {:?}", session_id);

        if let Some((_, user_id)) = self.session_to_user.remove(session_id) {
            self.user_sessions.remove(&user_id);
            info!("User session removed for session: {:?}", session_id);
        } else {
            warn!("Session not found: {:?}", session_id);
        }

        Ok(())
    }

    /// 获取用户ID通过会话ID
    pub fn get_user_id_by_session(&self, session_id: &SessionId) -> Option<u64> {
        self.session_to_user
            .get(session_id)
            .map(|entry| *entry.value())
    }

    /// 更新用户活动时间
    pub async fn update_user_activity(&self, user_id: u64) -> Result<()> {
        if let Some(user_session) = self.user_sessions.get_mut(&user_id) {
            // 由于UserSession的字段不是可变的，我们需要重新创建
            let new_session = UserSession::new(
                user_session.user_id,
                user_session.session_id,
                user_session.device_info.clone(),
            );

            self.user_sessions.insert(user_id, Arc::new(new_session));
            debug!("Updated activity for user: {}", user_id);
        }

        Ok(())
    }

    /// 获取所有在线用户
    pub async fn get_online_users(&self) -> Vec<u64> {
        self.user_sessions
            .iter()
            .map(|entry| *entry.key())
            .collect()
    }

    /// 清理过期会话
    pub async fn cleanup_expired_sessions(&self, timeout_secs: u64) -> Result<()> {
        let mut expired_users = Vec::new();

        for entry in self.user_sessions.iter() {
            let user_id = *entry.key();
            let user_session = entry.value();

            if user_session.is_expired(timeout_secs) {
                expired_users.push(user_id);
            }
        }

        for user_id in expired_users {
            self.remove_user_session(user_id).await?;
            info!("Cleaned up expired session for user: {}", user_id);
        }

        Ok(())
    }

    /// 获取会话统计信息
    pub fn get_session_stats(&self) -> SessionStats {
        SessionStats {
            total_sessions: self.user_sessions.len(),
            active_users: self.user_sessions.len(),
        }
    }

    /// 检查用户是否在线
    pub fn is_user_online(&self, user_id: &u64) -> bool {
        self.user_sessions.contains_key(user_id)
    }
}

/// 会话信息
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// 会话ID
    pub session_id: String,
    /// 在线设备列表
    pub online_devices: Vec<DeviceInfo>,
}

/// 会话统计信息
#[derive(Debug, Clone)]
pub struct SessionStats {
    /// 总会话数
    pub total_sessions: usize,
    /// 活跃用户数
    pub active_users: usize,
}
