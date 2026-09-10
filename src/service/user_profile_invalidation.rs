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

//! 资料变更后的失效广播：**清缓存与发通知必须成对**。
//!
//! 只发通知不清缓存，等于把客户端叫醒来重读同一份旧数据（2026-09-09 生产故障）。
//! 顺序也不能反：先清，再叫人来读。
//!
//! 这段逻辑原本只存在于 admin HTTP 路由里。自助改资料是第二个写入口，照抄一份
//! 就意味着两份实现各自演化——所以收在这里，两个入口调同一个函数。

use std::collections::BTreeSet;
use std::sync::Arc;

use tracing::warn;

use crate::infra::{CacheManager, ConnectionManager};
use crate::service::{ChannelService, EntityInvalidationPublisher, FriendService};

/// 收件人 = 本人 + 好友 + 同频道成员。
///
/// 本人必须在内：多端要靠这条通知刷新，而"改自己的资料"恰恰是本人最需要看到的。
pub fn profile_invalidation_recipients(
    user_id: u64,
    friend_ids: impl IntoIterator<Item = u64>,
    channel_member_ids: impl IntoIterator<Item = Vec<u64>>,
) -> BTreeSet<u64> {
    let mut recipients = BTreeSet::from([user_id]);
    recipients.extend(friend_ids);
    for members in channel_member_ids {
        recipients.extend(members);
    }
    recipients.remove(&0);
    recipients
}

/// 清掉该用户的资料缓存，然后向所有能看到他的人广播 user 实体失效。
///
/// 资料写入已经提交，这里是**尽力而为**的控制面提示：entity 行与 sync_version 才是
/// 权威，推送失败由重连 / 前台进入时的实体增量同步补齐。
pub async fn invalidate_user_profile_everywhere(
    user_id: u64,
    cache_manager: &CacheManager,
    friend_service: &FriendService,
    channel_service: &ChannelService,
    connection_manager: Arc<ConnectionManager>,
) {
    if let Err(error) = cache_manager.invalidate_user_profile(user_id).await {
        warn!(user_id, %error, "profile cache invalidation failed");
    }

    let (friends, channels) = tokio::join!(friend_service.get_friends(user_id), async {
        channel_service.get_user_channels(user_id).await.channels
    });
    let friend_ids = match friends {
        Ok(ids) => ids,
        Err(error) => {
            warn!(
                user_id,
                %error,
                "profile invalidation friend recipient resolution failed"
            );
            Vec::new()
        }
    };
    let recipients = profile_invalidation_recipients(
        user_id,
        friend_ids,
        channels.into_iter().map(|channel| channel.get_member_ids()),
    );

    let publisher = EntityInvalidationPublisher::new(connection_manager);
    if let Err(error) = publisher
        .publish_to_users(
            recipients,
            vec![privchat_protocol::EntityInvalidation {
                entity_type: "user".to_string(),
                entity_id: Some(user_id.to_string()),
                // user 实体的 scope 恒为目标 user id：即使可见性是从频道来的，
                // 它也不是 channel id。
                scope: Some(user_id.to_string()),
                target_version: 0,
                mutation_hint: privchat_protocol::EntityMutationHint::Upsert,
            }],
        )
        .await
    {
        warn!(user_id, %error, "profile invalidation dispatch failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 本人必须收到自己的失效通知——多端刷新全靠它。
    #[test]
    fn recipients_always_include_the_user() {
        let r = profile_invalidation_recipients(7, vec![], vec![]);
        assert_eq!(r, BTreeSet::from([7]));
    }

    /// uid 0 是占位/系统位，不是收件人。
    #[test]
    fn zero_is_not_a_recipient() {
        let r = profile_invalidation_recipients(7, vec![0, 8], vec![vec![0, 9]]);
        assert_eq!(r, BTreeSet::from([7, 8, 9]));
    }

    #[test]
    fn friends_and_channel_members_are_merged_without_duplicates() {
        let r = profile_invalidation_recipients(7, vec![8, 9], vec![vec![9, 10], vec![10, 11]]);
        assert_eq!(r, BTreeSet::from([7, 8, 9, 10, 11]));
    }
}
