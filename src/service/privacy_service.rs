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

use crate::error::{Result, ServerError};
use crate::infra::CacheManager;
use crate::model::privacy::{UserDetailSource, UserPrivacySettings};
use crate::service::ChannelService;
use crate::service::FriendService;
use chrono::Utc;
use std::sync::Arc;

/// 隐私和权限验证服务
pub struct PrivacyService {
    cache_manager: Arc<CacheManager>,
    channel_service: Arc<ChannelService>,
    friend_service: Arc<FriendService>,
}

impl PrivacyService {
    pub fn new(
        cache_manager: Arc<CacheManager>,
        channel_service: Arc<ChannelService>,
        friend_service: Arc<FriendService>,
    ) -> Self {
        Self {
            cache_manager,
            channel_service,
            friend_service,
        }
    }

    /// 验证查看用户资料的权限
    pub async fn validate_detail_access(
        &self,
        searcher_id: u64,
        target_id: u64,
        source: UserDetailSource,
    ) -> Result<()> {
        // 1. 如果是好友，直接允许（最高权限）
        if self.friend_service.is_friend(searcher_id, target_id).await {
            tracing::debug!("✅ 好友关系验证通过: {} -> {}", searcher_id, target_id);
            return Ok(());
        }

        // 2. 根据来源类型验证
        match source {
            UserDetailSource::Search { search_session_id } => {
                self.validate_search_source(searcher_id, target_id, search_session_id)
                    .await?;
            }
            UserDetailSource::Group { group_id } => {
                self.validate_group_source(searcher_id, target_id, group_id)
                    .await?;
            }
            UserDetailSource::Friend { friend_id: _ } => {
                // 好友来源已在第一步验证，这里不需要额外验证
            }
            UserDetailSource::CardShare { share_id } => {
                self.validate_card_share_source(searcher_id, target_id, share_id)
                    .await?;
            }
        }

        Ok(())
    }

    /// 验证搜索来源
    async fn validate_search_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        search_session_id: u64,
    ) -> Result<()> {
        // 1. 验证搜索记录
        let record = self
            .cache_manager
            .get_search_record(search_session_id)
            .await?
            .ok_or_else(|| {
                ServerError::NotFound(format!("Search record not found: {}", search_session_id))
            })?;

        tracing::info!("🔍 验证搜索记录: search_session_id={}, record.searcher_id={}, record.target_id={}, searcher_id={}, target_id={}", 
            search_session_id, record.searcher_id, record.target_id, searcher_id, target_id);

        // 2. 验证搜索者
        if record.searcher_id != searcher_id {
            return Err(ServerError::Forbidden(
                "Search record does not belong to this user".to_string(),
            ));
        }

        // 3. 验证目标用户
        if record.target_id != target_id {
            tracing::warn!(
                "❌ 目标用户不匹配: record.target_id={}, target_id={}",
                record.target_id,
                target_id
            );
            return Err(ServerError::Forbidden(
                "Search record target does not match".to_string(),
            ));
        }

        // 4. 验证是否过期
        if record.is_expired() {
            return Err(ServerError::Forbidden(
                "Search record has expired".to_string(),
            ));
        }

        // 5. 验证隐私设置：是否允许被搜索
        let privacy = self
            .cache_manager
            .get_privacy_settings(target_id)
            .await?
            .unwrap_or_else(|| UserPrivacySettings::new(target_id));

        // 检查是否允许被搜索（至少一种搜索方式允许）
        if !privacy.allow_search_by_username
            && !privacy.allow_search_by_phone
            && !privacy.allow_search_by_email
        {
            return Err(ServerError::Forbidden(
                "User does not allow being searched".to_string(),
            ));
        }

        Ok(())
    }

    /// 验证群组来源
    async fn validate_group_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        group_id: u64,
    ) -> Result<()> {
        // 1. 验证查看者是否在群中
        let channel = self.channel_service.get_channel(&group_id).await?;
        if !channel.members.contains_key(&searcher_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                searcher_id, group_id
            )));
        }

        // 2. 验证被查看者是否在群中
        if !channel.members.contains_key(&target_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                target_id, group_id
            )));
        }

        // 3. 验证隐私设置：是否允许通过群添加好友
        let privacy = self
            .cache_manager
            .get_privacy_settings(target_id)
            .await?
            .unwrap_or_else(|| UserPrivacySettings::new(target_id));

        if !privacy.allow_add_by_group {
            return Err(ServerError::Forbidden(
                "User does not allow being added via group".to_string(),
            ));
        }

        Ok(())
    }

    /// 验证名片分享来源
    async fn validate_card_share_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        share_id: u64,
    ) -> Result<()> {
        // 1. 获取分享记录
        let record = self
            .cache_manager
            .get_card_share(share_id)
            .await?
            .ok_or_else(|| {
                ServerError::NotFound(format!("Card share record not found: {}", share_id))
            })?;

        // 2. 验证接收者
        if record.receiver_id != searcher_id {
            return Err(ServerError::Forbidden(
                "Card share record does not belong to this user".to_string(),
            ));
        }

        // 3. 验证目标用户
        if record.target_user_id != target_id {
            return Err(ServerError::Forbidden(
                "Card share record target does not match".to_string(),
            ));
        }

        // 4. 验证是否已被使用
        if record.used {
            return Err(ServerError::Forbidden(
                "Card share record has already been used".to_string(),
            ));
        }

        Ok(())
    }

    /// 获取或创建默认隐私设置
    pub async fn get_or_create_privacy_settings(
        &self,
        user_id: u64,
    ) -> Result<UserPrivacySettings> {
        if let Some(settings) = self.cache_manager.get_privacy_settings(user_id).await? {
            Ok(settings)
        } else {
            let settings = UserPrivacySettings::new(user_id);
            self.cache_manager
                .set_privacy_settings(user_id, settings.clone())
                .await?;
            Ok(settings)
        }
    }

    /// 更新隐私设置
    pub async fn update_privacy_settings(
        &self,
        user_id: u64,
        updates: PrivacySettingsUpdate,
    ) -> Result<UserPrivacySettings> {
        // 获取现有设置或创建默认设置
        let mut settings = self.get_or_create_privacy_settings(user_id).await?;

        // 应用更新
        if let Some(allow_add_by_group) = updates.allow_add_by_group {
            settings.allow_add_by_group = allow_add_by_group;
        }
        if let Some(allow_search_by_phone) = updates.allow_search_by_phone {
            settings.allow_search_by_phone = allow_search_by_phone;
        }
        if let Some(allow_search_by_username) = updates.allow_search_by_username {
            settings.allow_search_by_username = allow_search_by_username;
        }
        if let Some(allow_search_by_email) = updates.allow_search_by_email {
            settings.allow_search_by_email = allow_search_by_email;
        }
        if let Some(allow_search_by_qrcode) = updates.allow_search_by_qrcode {
            settings.allow_search_by_qrcode = allow_search_by_qrcode;
        }
        if let Some(allow_view_by_non_friend) = updates.allow_view_by_non_friend {
            settings.allow_view_by_non_friend = allow_view_by_non_friend;
        }
        if let Some(allow_receive_message_from_non_friend) =
            updates.allow_receive_message_from_non_friend
        {
            settings.allow_receive_message_from_non_friend = allow_receive_message_from_non_friend;
        }

        // 更新更新时间
        settings.updated_at = Utc::now();

        // 保存到缓存
        self.cache_manager
            .set_privacy_settings(user_id, settings.clone())
            .await?;

        Ok(settings)
    }
}

/// 隐私设置更新（部分更新）
#[derive(Debug, Clone, Default)]
pub struct PrivacySettingsUpdate {
    pub allow_add_by_group: Option<bool>,
    pub allow_search_by_phone: Option<bool>,
    pub allow_search_by_username: Option<bool>,
    pub allow_search_by_email: Option<bool>,
    pub allow_search_by_qrcode: Option<bool>,
    pub allow_view_by_non_friend: Option<bool>,
    pub allow_receive_message_from_non_friend: Option<bool>,
}
