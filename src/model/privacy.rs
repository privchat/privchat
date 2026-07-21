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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::infra::next_message_id;

/// 用户隐私设置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserPrivacySettings {
    pub user_id: u64,

    /// 是否允许通过群组添加好友
    pub allow_add_by_group: bool,

    /// 是否允许通过名片分享添加好友（PROFILE_VISIBILITY §2.5;老数据缺省 true）
    #[serde(default = "default_true")]
    pub allow_add_by_card: bool,

    /// 是否允许通过手机号搜索到自己
    pub allow_search_by_phone: bool,

    /// 是否允许通过用户名搜索到自己
    pub allow_search_by_username: bool,

    /// 是否允许通过邮箱搜索到自己
    pub allow_search_by_email: bool,

    /// 是否允许通过二维码搜索到自己
    pub allow_search_by_qrcode: bool,

    /// 是否允许非好友查看资料
    pub allow_view_by_non_friend: bool,

    /// 是否允许接收非好友消息（类似QQ/Telegram/Zalo，用于客服系统）
    /// 默认 true，允许接收非好友消息
    pub allow_receive_message_from_non_friend: bool,

    /// 更新时间
    pub updated_at: DateTime<Utc>,
}

impl UserPrivacySettings {
    /// 创建默认隐私设置（全部允许）
    pub fn new(user_id: u64) -> Self {
        Self {
            user_id,
            allow_add_by_group: true,
            allow_add_by_card: true,
            allow_search_by_phone: true,
            allow_search_by_username: true,
            allow_search_by_email: true,
            allow_search_by_qrcode: true,
            allow_view_by_non_friend: false, // 默认不允许非好友查看
            allow_receive_message_from_non_friend: true, // 默认允许接收非好友消息（类似QQ/Telegram/Zalo）
            updated_at: Utc::now(),
        }
    }

    /// 检查是否允许被搜索（根据搜索类型）
    pub fn allows_search(&self, search_type: SearchType) -> bool {
        match search_type {
            SearchType::Phone => self.allow_search_by_phone,
            SearchType::Username => self.allow_search_by_username,
            SearchType::Email => self.allow_search_by_email,
            SearchType::Qrcode => self.allow_search_by_qrcode,
        }
    }
}

/// 搜索类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchType {
    Phone,
    Username,
    Email,
    Qrcode,
}

fn default_true() -> bool {
    true
}

/// 用户资料查看来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UserDetailSource {
    /// 搜索来源
    /// source_id: 搜索会话ID
    Search { search_session_id: u64 },

    /// 群组来源
    /// source_id: 群ID
    Group { group_id: u64 },

    /// 好友来源
    /// source_id: 好友的 user_id（可选）
    Friend { friend_id: Option<u64> },

    /// 名片分享来源
    /// source_id: 分享ID
    CardShare { share_id: u64 },

    /// 临时会话来源（聊天界面查看对方资料）
    /// source_id: channel_id
    Conversation { channel_id: u64 },
}

/// 搜索记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchRecord {
    /// 搜索会话ID（唯一标识）
    pub search_session_id: u64,

    /// 搜索者
    pub searcher_id: u64,

    /// 被搜索的用户
    pub target_id: u64,

    /// 命中方式（PROFILE_VISIBILITY D1:仅 by_username 精确命中解锁 username
    /// 回显）。老缓存记录缺省 None,按"不解锁"处理。
    #[serde(default)]
    pub hit_by: Option<SearchType>,

    /// 创建时间
    pub created_at: DateTime<Utc>,

    /// 过期时间（默认1小时后）
    pub expires_at: DateTime<Utc>,
}

impl SearchRecord {
    /// 创建新的搜索记录
    pub fn new(searcher_id: u64, target_id: u64, hit_by: Option<SearchType>) -> Self {
        let search_session_id = next_message_id();
        Self {
            search_session_id,
            searcher_id,
            target_id,
            hit_by,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(1), // 1小时后过期
        }
    }

    /// 检查是否过期
    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}

/// 名片分享记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardShareRecord {
    /// 分享ID（唯一标识）
    pub share_id: u64,

    /// 分享者（必须是目标用户的好友）
    pub sharer_id: u64,

    /// 被分享的用户（名片的主人）
    pub target_user_id: u64,

    /// 接收者（被分享给的人）
    pub receiver_id: u64,

    /// 创建时间
    pub created_at: DateTime<Utc>,

    /// 是否已被使用（添加好友）
    pub used: bool,

    /// 使用时间（如果已使用）
    pub used_at: Option<DateTime<Utc>>,

    /// 使用者的 user_id
    pub used_by: Option<u64>,
}

impl CardShareRecord {
    /// 创建新的名片分享记录
    pub fn new(share_id: u64, sharer_id: u64, target_user_id: u64, receiver_id: u64) -> Self {
        Self {
            share_id,
            sharer_id,
            target_user_id,
            receiver_id,
            created_at: Utc::now(),
            used: false,
            used_at: None,
            used_by: None,
        }
    }

    /// 标记为已使用
    pub fn mark_as_used(&mut self, user_id: u64) {
        self.used = true;
        self.used_at = Some(Utc::now());
        self.used_by = Some(user_id);
    }
}

/// 好友申请来源
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FriendRequestSource {
    /// 通过搜索添加
    Search { search_session_id: u64 },

    /// 通过群组添加
    Group { group_id: u64 },

    /// 通过名片分享添加
    CardShare { share_id: u64 },

    /// 通过二维码添加
    Qrcode { qrcode: String },

    /// 通过手机号添加
    Phone { phone: String },

    /// 通过聊天会话添加（1v1 / 群聊页打开对方资料后添加）
    Conversation { channel_id: u64 },
}

/// 查看凭证(PROFILE_VISIBILITY §2.5.1)。
///
/// `account/user/detail` 来源校验通过后签发;`friend/apply` 凭 grant_id 放行,
/// 不再重跑来源校验。detail 时刻的判定快照(TTL 窗口内状态漂移按快照,唯
/// 黑名单在 apply 时刻重验)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProfileViewGrant {
    pub grant_id: u64,
    pub viewer_id: u64,
    pub target_id: u64,
    /// 来源快照(落申请记录用,供审计/"通过群聊添加"展示)
    pub source_type: String,
    pub source_id: String,
    /// detail 时刻的加好友判定
    pub can_add_friend: bool,
    /// can_add_friend=false 时的原因:group_policy / personal_privacy /
    /// blacklist / already_friend
    pub deny_reason: Option<String>,
    /// by_username 精确搜索来源为 true(D1 username 回显解锁)
    pub username_unlocked: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl ProfileViewGrant {
    pub const TTL_MINUTES: i64 = 10;

    pub fn new(
        viewer_id: u64,
        target_id: u64,
        source_type: String,
        source_id: String,
        can_add_friend: bool,
        deny_reason: Option<String>,
        username_unlocked: bool,
    ) -> Self {
        Self {
            grant_id: next_message_id(),
            viewer_id,
            target_id,
            source_type,
            source_id,
            can_add_friend,
            deny_reason,
            username_unlocked,
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(Self::TTL_MINUTES),
        }
    }

    pub fn is_expired(&self) -> bool {
        Utc::now() > self.expires_at
    }
}
