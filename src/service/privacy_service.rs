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

/// detail 访问裁决(PROFILE_VISIBILITY §2.5):来源真伪已通过,
/// can_add_friend/username_unlocked 是 detail 时刻的判定快照。
#[derive(Debug, Clone)]
pub struct DetailAccessVerdict {
    pub is_friend: bool,
    pub can_add_friend: bool,
    pub deny_reason: Option<&'static str>,
    pub username_unlocked: bool,
}

impl DetailAccessVerdict {
    fn viewable_and_addable() -> Self {
        Self {
            is_friend: false,
            can_add_friend: true,
            deny_reason: None,
            username_unlocked: false,
        }
    }

    fn view_only(reason: &'static str) -> Self {
        Self {
            is_friend: false,
            can_add_friend: false,
            deny_reason: Some(reason),
            username_unlocked: false,
        }
    }
}

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

    /// 评估查看用户资料的权限(PROFILE_VISIBILITY §2.5)。
    ///
    /// 两层语义分离:
    ///   - **来源真伪**不通过 → Err(拒绝整个 detail 请求);
    ///   - **加好友权限**(群策略/个人「添加我的方式」开关)不通过 →
    ///     仍可查看公开投影,只是 verdict.can_add_friend=false + deny_reason。
    ///
    /// username_unlocked 仅在 by_username 精确搜索来源(D1)或好友关系下为 true。
    pub async fn evaluate_detail_access(
        &self,
        searcher_id: u64,
        target_id: u64,
        source: UserDetailSource,
    ) -> Result<DetailAccessVerdict> {
        // 好友是最高权限:来源无需再验,username 可见,不能重复添加。
        if self.friend_service.is_friend(searcher_id, target_id).await {
            tracing::debug!("✅ 好友关系验证通过: {} -> {}", searcher_id, target_id);
            return Ok(DetailAccessVerdict {
                is_friend: true,
                can_add_friend: false,
                deny_reason: Some("already_friend"),
                username_unlocked: true,
            });
        }

        match source {
            UserDetailSource::Search { search_session_id } => {
                self.evaluate_search_source(searcher_id, target_id, search_session_id)
                    .await
            }
            UserDetailSource::Group { group_id } => {
                self.evaluate_group_source(searcher_id, target_id, group_id)
                    .await
            }
            UserDetailSource::Friend { friend_id: _ } => {
                // 声称好友来源但上面已判非好友 → 来源不实。
                Err(ServerError::Forbidden(
                    "Friend source claimed but users are not friends".to_string(),
                ))
            }
            UserDetailSource::CardShare { share_id } => {
                self.validate_card_share_source(searcher_id, target_id, share_id)
                    .await?;
                let privacy = self
                    .cache_manager
                    .get_privacy_settings(target_id)
                    .await?
                    .unwrap_or_else(|| UserPrivacySettings::new(target_id));
                Ok(if privacy.allow_add_by_card {
                    DetailAccessVerdict::viewable_and_addable()
                } else {
                    DetailAccessVerdict::view_only("personal_privacy")
                })
            }
            UserDetailSource::Conversation { channel_id } => {
                // 会话来源:能进入会话即有查看资格(既有语义)。加好友权限对
                // 群会话套用 group 同款双闸(群聊 channel_id == group_id;DM 无
                // policy 行,natural 放行)。
                if let Ok(Some(policy)) = self.channel_service.get_group_policy(channel_id).await
                {
                    if !policy.allow_member_add_friend {
                        return Ok(DetailAccessVerdict::view_only("group_policy"));
                    }
                    let privacy = self
                        .cache_manager
                        .get_privacy_settings(target_id)
                        .await?
                        .unwrap_or_else(|| UserPrivacySettings::new(target_id));
                    if !privacy.allow_add_by_group {
                        return Ok(DetailAccessVerdict::view_only("personal_privacy"));
                    }
                }
                Ok(DetailAccessVerdict::viewable_and_addable())
            }
        }
    }

    /// 兼容入口:仅裁决"能否查看"(来源真伪)。旧调用点使用。
    pub async fn validate_detail_access(
        &self,
        searcher_id: u64,
        target_id: u64,
        source: UserDetailSource,
    ) -> Result<()> {
        self.evaluate_detail_access(searcher_id, target_id, source)
            .await
            .map(|_| ())
    }

    /// 评估搜索来源:真伪(归属/目标/过期)Err;命中方式映射个人搜索开关;
    /// by_username 命中解锁 username 回显。
    async fn evaluate_search_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        search_session_id: u64,
    ) -> Result<DetailAccessVerdict> {
        let record = self
            .cache_manager
            .get_search_record(search_session_id)
            .await?
            .ok_or_else(|| {
                ServerError::NotFound(format!("Search record not found: {}", search_session_id))
            })?;

        if record.searcher_id != searcher_id {
            return Err(ServerError::Forbidden(
                "Search record does not belong to this user".to_string(),
            ));
        }
        if record.target_id != target_id {
            return Err(ServerError::Forbidden(
                "Search record target does not match".to_string(),
            ));
        }
        if record.is_expired() {
            return Err(ServerError::Forbidden(
                "Search record has expired".to_string(),
            ));
        }

        let privacy = self
            .cache_manager
            .get_privacy_settings(target_id)
            .await?
            .unwrap_or_else(|| UserPrivacySettings::new(target_id));

        // 按真实命中方式查对应开关;老记录无 hit_by 时退回"任一允许"旧语义。
        let allowed = match record.hit_by {
            Some(t) => privacy.allows_search(t),
            None => {
                privacy.allow_search_by_username
                    || privacy.allow_search_by_phone
                    || privacy.allow_search_by_email
            }
        };
        if !allowed {
            return Err(ServerError::Forbidden(
                "User does not allow being searched".to_string(),
            ));
        }

        Ok(DetailAccessVerdict {
            is_friend: false,
            can_add_friend: true,
            deny_reason: None,
            username_unlocked: matches!(
                record.hit_by,
                Some(crate::model::privacy::SearchType::Username)
            ),
        })
    }

    /// 评估群来源:双方成员身份是真伪校验(Err);群策略与个人开关只影响
    /// can_add_friend(查看保留公开投影,微信同款)。
    async fn evaluate_group_source(
        &self,
        searcher_id: u64,
        target_id: u64,
        group_id: u64,
    ) -> Result<DetailAccessVerdict> {
        let channel = self.channel_service.get_channel(&group_id).await?;
        if !channel.members.contains_key(&searcher_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                searcher_id, group_id
            )));
        }
        if !channel.members.contains_key(&target_id) {
            return Err(ServerError::Forbidden(format!(
                "User {} is not a member of group {}",
                target_id, group_id
            )));
        }

        // 群策略:allow_member_add_friend=false 时,群主/管理员(任一方)豁免。
        let allow_member_add_friend = self
            .channel_service
            .get_group_policy(group_id)
            .await
            .ok()
            .flatten()
            .map(|p| p.allow_member_add_friend)
            .unwrap_or(true);
        if !allow_member_add_friend {
            use crate::model::channel::MemberRole;
            let is_privileged = |uid: &u64| {
                channel
                    .members
                    .get(uid)
                    .map(|m| matches!(m.role, MemberRole::Owner | MemberRole::Admin))
                    .unwrap_or(false)
            };
            if !is_privileged(&searcher_id) && !is_privileged(&target_id) {
                return Ok(DetailAccessVerdict::view_only("group_policy"));
            }
        }

        // 个人「添加我的方式」:允许通过群聊添加我(§2.5,20312)。
        let privacy = self
            .cache_manager
            .get_privacy_settings(target_id)
            .await?
            .unwrap_or_else(|| UserPrivacySettings::new(target_id));
        if !privacy.allow_add_by_group {
            return Ok(DetailAccessVerdict::view_only("personal_privacy"));
        }

        Ok(DetailAccessVerdict::viewable_and_addable())
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
