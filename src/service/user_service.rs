use std::sync::Arc;

use crate::error::AppError;
use crate::model::user::User;
use crate::service::auth_service::AuthService;

/// 用户服务
pub struct UserService {
    auth_service: Arc<AuthService>,
}

impl UserService {
    /// 创建新的用户服务
    pub fn new(auth_service: Arc<AuthService>) -> Self {
        Self { auth_service }
    }

    /// 获取用户信息
    pub async fn get_user(&self, user_id: u64) -> Result<Option<User>, AppError> {
        self.auth_service.get_user(user_id).await
    }

    /// 更新用户信息
    pub async fn update_user(&self, user: User) -> Result<(), AppError> {
        // 这里应该实现实际的用户更新逻辑
        // 暂时只记录日志
        tracing::info!("Updating user: {}", user.id);
        Ok(())
    }

    /// 根据用户ID获取用户
    pub async fn get_user_by_id(&self, user_id: u64) -> Result<Option<User>, AppError> {
        self.auth_service.get_user(user_id).await
    }

    /// 搜索用户
    pub async fn search_users(&self, query: &str) -> Result<Vec<User>, AppError> {
        self.auth_service.search_users(query).await
    }

    /// 获取用户设备列表
    pub async fn get_user_devices(
        &self,
        user_id: u64,
    ) -> Result<Vec<crate::model::user::DeviceInfo>, AppError> {
        self.auth_service.get_user_devices(user_id).await
    }
}
