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

use crate::infra::{auth_whitelist, SessionManager};
use msgtrans::SessionId;
use privchat_protocol::protocol::MessageType;
use privchat_protocol::ErrorCode;
use std::sync::Arc;

/// 认证结果类型
pub type AuthResult<T> = std::result::Result<T, ErrorCode>;

/// 认证中间件
///
/// 负责检查消息类型和 RPC 路由的访问权限
pub struct AuthMiddleware {
    session_manager: Arc<SessionManager>,
}

impl AuthMiddleware {
    /// 创建新的认证中间件
    pub fn new(session_manager: Arc<SessionManager>) -> Self {
        Self { session_manager }
    }

    /// 检查消息类型是否有权限访问
    ///
    /// # 参数
    /// - msg_type: 消息类型
    /// - session_id: 会话 ID
    ///
    /// # 返回
    /// - Ok(Some(user_id)) - 已认证，返回用户 ID
    /// - Ok(None) - 匿名访问（在白名单中）
    /// - Err(ErrorCode) - 需要认证但未认证
    pub async fn check_message_type(
        &self,
        msg_type: &MessageType,
        session_id: &SessionId,
    ) -> AuthResult<Option<String>> {
        // 1. 检查是否在白名单中
        if auth_whitelist::is_anonymous_message_type(msg_type) {
            tracing::debug!(
                "✅ 消息类型 {:?} 在白名单中，允许匿名访问 (session={})",
                msg_type,
                session_id
            );
            return Ok(None);
        }

        // 2. 检查会话是否已认证
        match self.session_manager.get_user_id(session_id).await {
            Some(user_id) => {
                // 更新活跃时间
                self.session_manager.update_active_time(session_id).await;

                tracing::debug!(
                    "✅ 消息类型认证成功: type={:?}, session={}, user={}",
                    msg_type,
                    session_id,
                    user_id
                );

                Ok(Some(user_id))
            }
            None => {
                tracing::warn!(
                    "❌ 消息类型 {:?} 需要认证: session={} (未认证)",
                    msg_type,
                    session_id
                );

                Err(ErrorCode::AuthRequired)
            }
        }
    }

    /// 检查 RPC 路由是否有权限访问
    ///
    /// # 参数
    /// - route: RPC 路由，如 "message/send"
    /// - session_id: 会话 ID
    ///
    /// # 返回
    /// - Ok(Some(user_id)) - 已认证，返回用户 ID
    /// - Ok(None) - 匿名访问（在白名单中）
    /// - Err(ErrorCode) - 需要认证但未认证
    pub async fn check_rpc_route(
        &self,
        route: &str,
        session_id: &SessionId,
    ) -> AuthResult<Option<String>> {
        // 1. 检查是否在白名单中
        if auth_whitelist::is_anonymous_rpc_route(route) {
            tracing::debug!(
                "✅ RPC '{}' 在白名单中，允许匿名访问 (session={})",
                route,
                session_id
            );
            return Ok(None);
        }

        // 2. 检查会话是否已认证
        match self.session_manager.get_user_id(session_id).await {
            Some(user_id) => {
                // 更新活跃时间
                self.session_manager.update_active_time(session_id).await;

                tracing::debug!(
                    "✅ RPC 认证成功: route={}, session={}, user={}",
                    route,
                    session_id,
                    user_id
                );

                Ok(Some(user_id))
            }
            None => {
                tracing::warn!(
                    "❌ RPC '{}' 需要认证: session={} (未认证)",
                    route,
                    session_id
                );

                Err(ErrorCode::AuthRequired)
            }
        }
    }

    /// 获取会话的用户信息
    ///
    /// 不检查白名单，直接返回会话对应的用户 ID
    pub async fn get_user_id(&self, session_id: &SessionId) -> Option<String> {
        self.session_manager.get_user_id(session_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ImTokenClaims;
    use crate::infra::SessionManager;
    use chrono::{Duration, Utc};

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
    async fn test_anonymous_message_type() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager);
        let session_id = SessionId::new(1);

        // AuthorizationRequest 消息应该允许匿名访问
        let result = auth_middleware
            .check_message_type(&MessageType::AuthorizationRequest, &session_id)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // None 表示匿名访问
    }

    #[tokio::test]
    async fn test_authenticated_message_type() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager.clone());
        let session_id = SessionId::new(1);
        let claims = create_test_claims(1001, "device-1");

        // 绑定会话
        session_manager
            .bind_session(
                session_id.clone(),
                "1001".to_string(),
                "device-1".to_string(),
                claims,
            )
            .await;

        // SendMessageRequest 消息需要认证，应该返回 user_id
        let result = auth_middleware
            .check_message_type(&MessageType::SendMessageRequest, &session_id)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("1001".to_string()));
    }

    #[tokio::test]
    async fn test_unauthenticated_message_type() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager);
        let session_id = SessionId::new(1);

        // SendMessageRequest 消息需要认证，未认证应该失败
        let result = auth_middleware
            .check_message_type(&MessageType::SendMessageRequest, &session_id)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_anonymous_rpc_route() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager);
        let session_id = SessionId::new(1);

        // 系统健康检查 RPC 应该允许匿名访问
        let result = auth_middleware
            .check_rpc_route("system/health", &session_id)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), None); // None 表示匿名访问
    }

    #[tokio::test]
    async fn test_authenticated_rpc_route() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager.clone());
        let session_id = SessionId::new(1);
        let claims = create_test_claims(1001, "device-1");

        // 绑定会话
        session_manager
            .bind_session(
                session_id.clone(),
                "1001".to_string(),
                "device-1".to_string(),
                claims,
            )
            .await;

        // 发送消息 RPC 需要认证，应该返回 user_id
        let result = auth_middleware
            .check_rpc_route("message/send", &session_id)
            .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some("1001".to_string()));
    }

    #[tokio::test]
    async fn test_unauthenticated_rpc_route() {
        let session_manager = Arc::new(SessionManager::new(24));
        let auth_middleware = AuthMiddleware::new(session_manager);
        let session_id = SessionId::new(1);

        // 发送消息 RPC 需要认证，未认证应该失败
        let result = auth_middleware
            .check_rpc_route("message/send", &session_id)
            .await;

        assert!(result.is_err());
    }
}
