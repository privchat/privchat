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

//! RPC 辅助函数

use crate::infra::cache_manager::CachedUserProfile;
use crate::model::user::User;

/// 将 User 模型转换为 CachedUserProfile
///
/// username / display_name 未设置时**透传空串**，不再 fallback 为 `user_<uid>`——
/// 让 client 端按自己的 UX 决定如何显示空值（"未设置" / 隐藏 / 显示 uid 等）。
pub fn user_to_cached_profile(user: &User) -> CachedUserProfile {
    CachedUserProfile {
        user_id: user.id.to_string(),
        username: user.username.clone().unwrap_or_default(),
        nickname: user.display_name.clone().unwrap_or_default(),
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
    let user = user_repository
        .find_by_id(user_id)
        .await
        .map_err(|e| crate::error::ServerError::Internal(format!("查询用户失败: {}", e)))?;

    match user {
        Some(user) => {
            let profile = user_to_cached_profile(&user);
            // 3. 写入缓存
            let _ = cache_manager
                .set_user_profile(user.id, profile.clone())
                .await;
            Ok(Some(profile))
        }
        None => Ok(None),
    }
}

/// 查 user_type（0=普通 / 1=系统 / 2=机器人）。
///
/// 顺序：1~99 区段直接判内置 System User；缓存命中读 `CachedUserProfile.user_type`；
/// 缓存未命中走 `user_repository.find_by_id`。未找到的 user 返回 `None`——调用方
/// 自行决定是当成普通用户放行还是拒绝。
pub async fn lookup_user_type(
    user_id: u64,
    user_repository: &crate::repository::UserRepository,
    cache_manager: &crate::infra::CacheManager,
) -> Result<Option<i16>, crate::error::ServerError> {
    if crate::config::is_system_user(user_id) {
        return Ok(Some(1));
    }
    if let Ok(Some(profile)) = cache_manager.get_user_profile(user_id).await {
        return Ok(Some(profile.user_type));
    }
    let user = user_repository
        .find_by_id(user_id)
        .await
        .map_err(|e| crate::error::ServerError::Internal(format!("查询用户失败: {}", e)))?;
    Ok(user.map(|u| u.user_type))
}
