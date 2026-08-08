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

//! **「这个人能不能往这个会话里写一条消息」的唯一判定。**
//!
//! 🔴 这一层存在的理由：这套策略原先内联在 `send_message_handler` 里，于是
//! 每条**新的**写入路径都得自己重新想一遍——`message/forward` 第一版就只
//! 校验了「是不是目标会话成员」，被禁言、被拉黑、没有发言角色的用户都能
//! 从转发这条路把消息发出去。加一个写入口不该等于加一份权限实现。
//!
//! 判定顺序有意与历史行为一致，因为客户端按错误码做不同的提示与重试：
//!
//! 1. 是不是会话成员
//! 2. 群：个人禁言 → 全员禁言（群主/管理员豁免）→ 角色发言权限
//! 3. 私聊：好友直接放行；非好友再看双向拉黑、对方的「仅接收好友消息」

use std::sync::Arc;

use privchat_protocol::error_code::ErrorCode;

use crate::model::channel::{Channel, ChannelType, MemberPermissions, MemberRole};
use crate::service::{BlacklistService, ChannelService, FriendService, PrivacyService};

/// 拒绝理由。**每一条都带自己的错误码**——统一成 `PermissionDenied` 会让
/// 客户端无法区分「你被禁言了（等一会儿）」和「对方把你拉黑了（别再发了）」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendRefusal {
    NotAMember,
    /// 个人禁言。文案里带解禁时间，由 `mute_reject_message` 生成。
    MemberMuted(String),
    GroupAllMuted,
    RoleCannotSend,
    /// 对方拉黑了我。
    BlockedByPeer,
    /// 我拉黑了对方。
    PeerInMyBlacklist,
    /// 对方只接收好友消息。
    PeerRejectsNonFriends,
    /// 频道设置不允许该成员发言（如群 `allow_member_post=false`）。
    ChannelForbidsPosting,
}

impl SendRefusal {
    pub fn error_code(&self) -> ErrorCode {
        match self {
            SendRefusal::NotAMember => ErrorCode::PermissionDenied,
            SendRefusal::MemberMuted(_) => ErrorCode::MemberMuted,
            SendRefusal::GroupAllMuted => ErrorCode::GroupMuted,
            SendRefusal::RoleCannotSend => ErrorCode::PermissionDenied,
            SendRefusal::BlockedByPeer => ErrorCode::BlockedByUser,
            SendRefusal::PeerInMyBlacklist => ErrorCode::UserInBlacklist,
            SendRefusal::PeerRejectsNonFriends => ErrorCode::PermissionDenied,
            SendRefusal::ChannelForbidsPosting => ErrorCode::PermissionDenied,
        }
    }

    pub fn message(&self) -> String {
        match self {
            SendRefusal::NotAMember => "无权限访问此频道".to_string(),
            SendRefusal::MemberMuted(text) => text.clone(),
            SendRefusal::GroupAllMuted => "群组全员禁言中".to_string(),
            SendRefusal::RoleCannotSend => "您没有发送消息权限".to_string(),
            SendRefusal::BlockedByPeer => "您已被对方拉黑，无法发送消息".to_string(),
            SendRefusal::PeerInMyBlacklist => "您已拉黑该用户，无法发送消息".to_string(),
            SendRefusal::PeerRejectsNonFriends => {
                "对方设置了仅接收好友消息，无法发送".to_string()
            }
            SendRefusal::ChannelForbidsPosting => "无权限发送消息".to_string(),
        }
    }
}

/// 判定需要的服务。集中成一个结构体，免得每个调用点各传一串参数、
/// 漏传一个就悄悄少一道校验。
#[derive(Clone)]
pub struct SendAuthorizationDeps {
    pub channel_service: Arc<ChannelService>,
    pub friend_service: Arc<FriendService>,
    pub blacklist_service: Arc<BlacklistService>,
    pub privacy_service: Arc<PrivacyService>,
}

/// 判定 `sender_id` 能否向 `channel` 写入一条消息。
///
/// `Ok(())` = 放行。副作用只有一个：临时禁言到期时异步懒清理 DB/缓存，
/// 与历史行为一致（此处持成员引用，不能同步取写锁）。
pub async fn authorize_send_to_channel(
    deps: &SendAuthorizationDeps,
    channel: &Channel,
    sender_id: u64,
) -> Result<(), SendRefusal> {
    let member = channel.members.get(&sender_id);
    if member.is_none() {
        return Err(SendRefusal::NotAMember);
    }

    if channel.channel_type == ChannelType::Group {
        let member = member.expect("membership checked above");

        // 个人禁言。到期的禁言放行本条，并异步清理——同步取写锁会在这里持成员引用时死锁。
        if member.is_muted {
            let now = chrono::Utc::now();
            if crate::model::channel::mute_is_active(member.is_muted, member.mute_until, now) {
                return Err(SendRefusal::MemberMuted(
                    crate::model::channel::mute_reject_message(member.mute_until, now),
                ));
            }
            let channel_service = deps.channel_service.clone();
            let (lazy_channel_id, lazy_uid) = (channel.id, sender_id);
            tokio::spawn(async move {
                if let Err(e) = channel_service
                    .set_member_muted(&lazy_channel_id, &lazy_uid, false, None)
                    .await
                {
                    tracing::warn!(
                        "mute lazy-clear failed: channel={} uid={} err={}",
                        lazy_channel_id,
                        lazy_uid,
                        e
                    );
                }
            });
        }

        // 全员禁言：以 DB(privchat_groups.all_muted) 为真源，server 重启后仍生效，
        // 不依赖可能丢失的内存缓存。群主/管理员豁免，因此只对普通成员查询。
        let is_privileged = matches!(member.role, MemberRole::Owner | MemberRole::Admin);
        if !is_privileged {
            // 🔴 查询失败不能当成「没禁言」。`.ok().flatten()` 把数据库抖动
            // 变成一次放行——全员禁言期间只要 DB 抖一下，消息就发出去了。
            // 判定不出来时按**拒绝**处理（禁言是限制性策略，宁可多拦一条）。
            let all_muted = if let Some(group_id) = channel.group_id {
                match deps.channel_service.get_group_policy(group_id).await {
                    Ok(Some(policy)) => policy.all_muted,
                    // 群不存在：没有群策略可言，不拦。
                    Ok(None) => false,
                    Err(e) => {
                        tracing::error!("查询群 {group_id} 策略失败，按全员禁言处理: {e}");
                        true
                    }
                }
            } else {
                false
            };
            if all_muted {
                return Err(SendRefusal::GroupAllMuted);
            }
        }

        if !MemberPermissions::from_role(member.role).can_send_message {
            return Err(SendRefusal::RoleCannotSend);
        }
    }

    // 频道级设置（如群的 allow_member_post）。放在角色权限之后，与历史顺序一致。
    if !channel.can_user_post(&sender_id) {
        return Err(SendRefusal::ChannelForbidsPosting);
    }

    if channel.channel_type == ChannelType::Direct {
        let peer_id = channel
            .get_member_ids()
            .into_iter()
            .find(|id| *id != sender_id);

        if let Some(peer_id) = peer_id {
            // 好友直接放行，跳过拉黑与隐私检查（与历史行为一致）。
            if deps.friend_service.is_friend(sender_id, peer_id).await {
                return Ok(());
            }

            let (sender_blocks_peer, peer_blocks_sender) = deps
                .blacklist_service
                .check_mutual_block(sender_id, peer_id)
                .await
                .unwrap_or((false, false));
            if peer_blocks_sender {
                return Err(SendRefusal::BlockedByPeer);
            }
            if sender_blocks_peer {
                return Err(SendRefusal::PeerInMyBlacklist);
            }

            match deps
                .privacy_service
                .get_or_create_privacy_settings(peer_id)
                .await
            {
                Ok(settings) => {
                    if !settings.allow_receive_message_from_non_friend {
                        return Err(SendRefusal::PeerRejectsNonFriends);
                    }
                }
                Err(e) => {
                    // 与历史行为一致：隐私设置取不到时默认允许。
                    tracing::warn!("获取用户 {peer_id} 隐私设置失败: {e}，默认允许非好友消息");
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每种拒绝都有自己的错误码：客户端据此决定「稍后再试」还是「别再发了」。
    /// 全部压成 PermissionDenied 会让禁言和被拉黑长得一模一样。
    #[test]
    fn each_refusal_carries_its_own_error_code() {
        assert_eq!(
            SendRefusal::MemberMuted("x".into()).error_code(),
            ErrorCode::MemberMuted
        );
        assert_eq!(SendRefusal::GroupAllMuted.error_code(), ErrorCode::GroupMuted);
        assert_eq!(
            SendRefusal::BlockedByPeer.error_code(),
            ErrorCode::BlockedByUser
        );
        assert_eq!(
            SendRefusal::PeerInMyBlacklist.error_code(),
            ErrorCode::UserInBlacklist
        );
        assert_ne!(
            SendRefusal::BlockedByPeer.error_code(),
            SendRefusal::PeerInMyBlacklist.error_code(),
            "「对方拉黑我」与「我拉黑对方」是两件事，文案与后续动作都不同",
        );
    }

    /// 禁言文案带解禁时间，直接透传给用户。
    #[test]
    fn a_mute_refusal_keeps_the_generated_text() {
        let refusal = SendRefusal::MemberMuted("你已被禁言至 12:00".into());
        assert_eq!(refusal.message(), "你已被禁言至 12:00");
    }
}
