//! RPC 辅助函数

use crate::model::user::User;
use crate::infra::cache_manager::CachedUserProfile;

/// 将 User 模型转换为 CachedUserProfile
pub fn user_to_cached_profile(user: &User) -> CachedUserProfile {
    CachedUserProfile {
        user_id: user.id.to_string(),
        username: user.username.clone(),
        nickname: user.display_name.clone().unwrap_or_else(|| user.username.clone()),
        avatar_url: user.avatar_url.clone(),
        user_type: user.user_type,
        phone: user.phone.clone(),
        email: user.email.clone(),
    }
}

/// 从数据库获取用户资料（优先从缓存，缓存未命中则从数据库读取并写入缓存）
/// 支持通过 UUID 或 username 查找用户
pub async fn get_user_profile_with_fallback(
    user_id: u64,
    user_repository: &crate::repository::UserRepository,
    cache_manager: &crate::infra::CacheManager,
) -> Result<Option<CachedUserProfile>, crate::error::ServerError> {
    // 1. 先尝试从缓存读取
    if let Ok(Some(profile)) = cache_manager.get_user_profile(user_id).await {
        return Ok(Some(profile));
    }
    
    // 2. 缓存未命中，从数据库读取
    let user = user_repository.find_by_id(user_id).await
        .map_err(|e| crate::error::ServerError::Internal(format!("查询用户失败: {}", e)))?;
    
    match user {
        Some(user) => {
            let profile = user_to_cached_profile(&user);
            // 3. 写入缓存
            let _ = cache_manager.set_user_profile(user.id, profile.clone()).await;
            Ok(Some(profile))
        }
        None => Ok(None),
    }
}
