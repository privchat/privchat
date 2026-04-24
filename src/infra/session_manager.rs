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

use crate::auth::ImTokenClaims;
use chrono::{DateTime, Duration, Utc};
use msgtrans::SessionId;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// 会话信息
#[derive(Clone, Debug)]
pub struct SessionInfo {
    /// 用户 ID
    pub user_id: String,
    /// 设备 ID
    pub device_id: String,
    /// 认证时间
    pub authenticated_at: DateTime<Utc>,
    /// 最后活跃时间
    pub last_active_at: DateTime<Utc>,
    /// JWT 声明
    pub jwt_claims: ImTokenClaims,
    /// 客户端 pts（服务器维护，推送离线消息后更新）
    pub client_pts: u64,
    /// 是否已准备好接收补差和实时推送
    pub ready_for_push: bool,
}

/// 会话管理器
///
/// 负责管理 session_id 与 user_id 的映射关系，
/// 用于后续请求的认证和授权
#[derive(Clone)]
pub struct SessionManager {
    /// 会话映射：session_id -> SessionInfo
    sessions: Arc<RwLock<HashMap<SessionId, SessionInfo>>>,
    /// 会话超时时间
    session_timeout: Duration,
}

impl SessionManager {
    /// 创建新的会话管理器
    ///
    /// # 参数
    /// - session_timeout_hours: 会话超时时间（小时）
    pub fn new(session_timeout_hours: i64) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            session_timeout: Duration::hours(session_timeout_hours),
        }
    }

    /// 绑定会话
    ///
    /// 在用户成功认证后调用，建立 session_id 与 user_id 的映射
    pub async fn bind_session(
        &self,
        session_id: SessionId,
        user_id: String,
        device_id: String,
        jwt_claims: ImTokenClaims,
    ) {
        let info = SessionInfo {
            user_id: user_id.clone(),
            device_id: device_id.clone(),
            authenticated_at: Utc::now(),
            last_active_at: Utc::now(),
            jwt_claims,
            client_pts: 0,         // 初始化为 0，推送离线消息后更新
            ready_for_push: false, // AUTH 后默认未 READY
        };

        self.sessions.write().await.insert(session_id.clone(), info);

        tracing::info!(
            "✅ 会话绑定成功: session={}, user={}, device={}, client_pts=0",
            session_id,
            user_id,
            device_id
        );
    }

    /// 更新客户端的 client_pts（推送离线消息后调用）
    pub async fn update_client_pts(&self, session_id: &SessionId, client_pts: u64) {
        if let Some(info) = self.sessions.write().await.get_mut(session_id) {
            info.client_pts = client_pts;
            info.last_active_at = Utc::now();
        }
    }

    /// 获取客户端的 client_pts
    pub async fn get_client_pts(&self, session_id: &SessionId) -> Option<u64> {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|info| info.client_pts)
    }

    /// 标记会话已 READY（幂等）
    ///
    /// 返回值:
    /// - true: 首次从 not-ready 转为 ready
    /// - false: 原本已经 ready 或 session 不存在
    pub async fn mark_ready_for_push(&self, session_id: &SessionId) -> bool {
        if let Some(info) = self.sessions.write().await.get_mut(session_id) {
            let transitioned = !info.ready_for_push;
            info.ready_for_push = true;
            info.last_active_at = Utc::now();
            transitioned
        } else {
            false
        }
    }

    /// 检查会话是否已 READY
    pub async fn is_ready_for_push(&self, session_id: &SessionId) -> bool {
        self.sessions
            .read()
            .await
            .get(session_id)
            .map(|info| info.ready_for_push)
            .unwrap_or(false)
    }

    /// 获取用户 ID
    ///
    /// 根据 session_id 查询对应的 user_id
    ///
    /// # 返回
    /// - Some(user_id) - 会话已认证
    /// - None - 会话未认证或不存在
    pub async fn get_user_id(&self, session_id: &SessionId) -> Option<String> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).map(|info| info.user_id.clone())
    }

    /// 获取完整会话信息
    pub async fn get_session_info(&self, session_id: &SessionId) -> Option<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned()
    }

    /// 更新活跃时间
    ///
    /// 在每次请求时调用，用于会话保活
    pub async fn update_active_time(&self, session_id: &SessionId) {
        if let Some(info) = self.sessions.write().await.get_mut(session_id) {
            info.last_active_at = Utc::now();
        }
    }

    /// 解绑会话
    ///
    /// 在用户断开连接或登出时调用
    pub async fn unbind_session(&self, session_id: &SessionId) {
        if let Some(info) = self.sessions.write().await.remove(session_id) {
            tracing::info!(
                "🔓 会话解绑: session={}, user={}, device={}",
                session_id,
                info.user_id,
                info.device_id
            );
        }
    }

    /// 清理过期会话
    ///
    /// 定期调用，清理长时间未活跃的会话
    pub async fn cleanup_expired_sessions(&self) -> usize {
        let now = Utc::now();
        let mut sessions = self.sessions.write().await;

        let before_count = sessions.len();

        sessions.retain(|session_id, info| {
            let elapsed = now.signed_duration_since(info.last_active_at);
            let expired = elapsed > self.session_timeout;

            if expired {
                tracing::info!(
                    "🧹 清理过期会话: session={}, user={}, inactive_for={}h",
                    session_id,
                    info.user_id,
                    elapsed.num_hours()
                );
            }

            !expired
        });

        let cleaned = before_count - sessions.len();

        if cleaned > 0 {
            tracing::info!("🧹 会话清理完成: 清理了 {} 个过期会话", cleaned);
        }

        cleaned
    }

    /// 列出用户的所有会话
    ///
    /// 用于设备管理，查看用户在哪些设备上登录
    pub async fn list_user_sessions(&self, user_id: u64) -> Vec<(SessionId, SessionInfo)> {
        self.sessions
            .read()
            .await
            .iter()
            .filter(|(_, info)| {
                info.user_id
                    .parse::<u64>()
                    .map(|id| id == user_id)
                    .unwrap_or(false)
            })
            .map(|(sid, info)| (sid.clone(), info.clone()))
            .collect()
    }

    /// 获取会话总数
    pub async fn session_count(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// 获取在线用户数
    pub async fn online_user_count(&self) -> usize {
        let sessions = self.sessions.read().await;
        let mut users = std::collections::HashSet::new();
        for info in sessions.values() {
            users.insert(&info.user_id);
        }
        users.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ImTokenClaims;

    fn create_test_claims(user_id: u64, device_id: &str) -> ImTokenClaims {
        ImTokenClaims {
            sub: user_id.to_string(),
            device_id: device_id.to_string(),
            exp: (Utc::now() + Duration::hours(24)).timestamp(),
            iat: Utc::now().timestamp(),
            jti: uuid::Uuid::new_v4().to_string(),
            iss: "test".to_string(),
            aud: "test".to_string(),
            business_system_id: "test-business".to_string(),
            app_id: "test-app".to_string(),
            session_version: 1,
            typ: "access".to_string(),
        }
    }

    #[tokio::test]
    async fn test_bind_and_get_session() {
        let manager = SessionManager::new(24);
        let session_id = SessionId::new(1);
        let claims = create_test_claims(1001, "device-1");

        // 绑定会话
        manager
            .bind_session(
                session_id.clone(),
                "1001".to_string(),
                "device-1".to_string(),
                claims,
            )
            .await;

        // 获取用户 ID
        let user_id = manager.get_user_id(&session_id).await;
        assert_eq!(user_id, Some("1001".to_string()));
    }

    #[tokio::test]
    async fn test_unbind_session() {
        let manager = SessionManager::new(24);
        let session_id = SessionId::new(1);
        let claims = create_test_claims(1001, "device-1");

        manager
            .bind_session(
                session_id.clone(),
                "1001".to_string(),
                "device-1".to_string(),
                claims,
            )
            .await;

        // 解绑会话
        manager.unbind_session(&session_id).await;

        // 验证已解绑
        let user_id = manager.get_user_id(&session_id).await;
        assert_eq!(user_id, None);
    }

    #[tokio::test]
    async fn test_list_user_sessions() {
        let manager = SessionManager::new(24);
        let claims = create_test_claims(1001, "device-1");

        // 同一用户的多个会话
        manager
            .bind_session(
                SessionId::new(1),
                "1001".to_string(),
                "device-1".to_string(),
                claims.clone(),
            )
            .await;

        manager
            .bind_session(
                SessionId::new(2),
                "1001".to_string(),
                "device-2".to_string(),
                claims.clone(),
            )
            .await;

        // 列出用户的所有会话
        let sessions = manager.list_user_sessions(1001).await;
        assert_eq!(sessions.len(), 2);
    }
}
