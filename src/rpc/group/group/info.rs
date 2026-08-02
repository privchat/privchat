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

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::group::GroupInfoRequest;
use serde_json::{json, Value};

/// 角色的**线上字符串**，走协议里的权威定义。
///
/// 从前这里是 `format!("{:?}", role).to_lowercase()` —— 用 Debug 输出拼线上契约。
/// 它今天恰好对，但枚举改个变体名就会静默改变线上值；而首字母大写的那一版
/// 正是让三端 `canManage` 恒 false、群主在所有端看不到管理入口的那次事故。
/// server 内部 [`MemberRole`] → 协议权威枚举 [`GroupMemberRole`]。
///
/// 两套编号**不一样**：server 模型与 DB 列是 Owner=0/Admin=1/Member=2，
/// 协议冻结的是 Member=0/Owner=1/Admin=2（0 必须是权限最低的，见枚举文档）。
/// 这里就是那个必须存在的转换点——服务端是唯一需要转换的一侧，客户端直接吃协议值。
fn to_wire_role(
    role: crate::model::channel::MemberRole,
) -> privchat_protocol::rpc::group::role::GroupMemberRole {
    use crate::model::channel::MemberRole;
    use privchat_protocol::rpc::group::role::GroupMemberRole;
    match role {
        MemberRole::Owner => GroupMemberRole::Owner,
        MemberRole::Admin => GroupMemberRole::Admin,
        MemberRole::Member => GroupMemberRole::Member,
    }
}

/// 角色的线上**字符串**（恒小写）。
///
/// 从前这里是 `format!("{:?}", role).to_lowercase()` —— 用 Debug 输出拼线上契约。
/// 它今天恰好对，但枚举改个变体名就会静默改变线上值；而首字母大写的那一版
/// 正是让三端 `canManage` 恒 false、群主在所有端看不到管理入口的那次事故。
fn wire_role(role: crate::model::channel::MemberRole) -> String {
    to_wire_role(role).as_str().to_string()
}

/// 请求者在本群的角色，小写契约（"owner"/"admin"/"member"）；非成员返回 ""。
///
/// **小写是硬契约**：`format!("{:?}")` 的首字母大写曾让三端的 `canManage` 恒为 false，
/// 群主在所有端都看不到任何管理入口。
fn my_role_of(members: &[crate::model::ChannelMember], requester_id: u64) -> String {
    members
        .iter()
        .find(|member| member.user_id == requester_id)
        .map(|member| wire_role(member.role))
        .unwrap_or_default()
}

/// 群主 uid。`channel.creator_id` 在 hydrate 出来的 channel 上是 0——用它下发，
/// 客户端的【群主】标签就永远不会显示。以成员表里的 Owner 为准，creator_id 兜底。
fn owner_id_of(members: &[crate::model::ChannelMember], creator_id: u64) -> u64 {
    members
        .iter()
        .find(|member| matches!(member.role, crate::model::channel::MemberRole::Owner))
        .map(|member| member.user_id)
        .unwrap_or(creator_id)
}

/// 管理员 uid（群主不在内，见 `owner_id`）。有界，够气泡打标签用。
fn admin_ids(members: &[crate::model::ChannelMember]) -> Vec<u64> {
    members
        .iter()
        .filter(|member| matches!(member.role, crate::model::channel::MemberRole::Admin))
        .map(|member| member.user_id)
        .collect()
}

/// 处理 群组信息 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupInfoRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let requester_id = request.user_id;

    tracing::debug!("🔧 查询群组信息: {}", group_id);

    // 获取群组信息
    match services.channel_service.get_channel(&group_id).await {
        Ok(channel) => {
            // 获取成员列表
            let members = match services
                .channel_service
                .get_channel_members(&group_id)
                .await
            {
                Ok(members) => members,
                Err(_) => Vec::new(),
            };

            // 创建默认统计信息（get_channel_stats 方法不存在，使用默认值）
            let stats = crate::model::channel::ChannelStats {
                channel_id: group_id,
                member_count: members.len() as u32,
                message_count: channel.message_count as u64,
                today_message_count: 0,
                active_member_count: 0,
                stats_time: chrono::Utc::now(),
            };

            Ok(json!({
                "status": "success",
                "group_info": {
                    "group_id": channel.id,
                    "name": channel.metadata.name,
                    "description": channel.metadata.description,
                    "avatar_url": channel.metadata.avatar_url,
                    "owner_id": owner_id_of(&members, channel.creator_id),
                    "created_at": channel.created_at.timestamp_millis(),
                    "updated_at": channel.updated_at.timestamp_millis(),
                    "member_count": stats.member_count,
                    "message_count": stats.message_count,
                    "is_archived": matches!(channel.status, crate::model::channel::ChannelStatus::Archived),
                    "tags": channel.metadata.tags,
                    "custom_fields": channel.metadata.custom_properties,
                    // 请求者自己的角色 —— 客户端据此判断能否管理本群，
                    // 不必再拉整份花名册在里面找自己（CHANNEL_SPEC §9.2.2）。
                    // 小写契约：Debug 的首字母大写曾把三端权限判定全打挂。
                    "my_role": my_role_of(&members, requester_id),
                    // 管理员有界（个位数），够气泡打【管理】标签用；群主见 owner_id。
                    "admin_user_ids": admin_ids(&members)
                },
                "timestamp": chrono::Utc::now().timestamp_millis()
            }))
        }
        Err(e) => {
            tracing::error!("❌ 查询群组信息失败: {}", e);
            Err(RpcError::not_found(format!("群组不存在: {}", group_id)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{admin_ids, my_role_of, owner_id_of};
    use crate::model::channel::{ChannelMember, MemberRole};

    fn member(user_id: u64, role: MemberRole) -> ChannelMember {
        ChannelMember::new(user_id, role)
    }

    /// 角色必须小写下发。Debug 的 "Owner" 曾让三端 canManage 恒 false，
    /// 群主在所有端都看不到管理入口。
    #[test]
    fn my_role_is_lowercase() {
        let members = vec![member(7, MemberRole::Owner), member(8, MemberRole::Member)];
        assert_eq!(my_role_of(&members, 7), "owner");
        assert_eq!(my_role_of(&members, 8), "member");
    }

    /// 非成员不能拿到角色——否则「我不在这个群」也会被算成有权限。
    #[test]
    fn a_stranger_has_no_role() {
        let members = vec![member(7, MemberRole::Owner)];
        assert_eq!(my_role_of(&members, 999), "");
    }

    /// 群主取自成员表。hydrate 出来的 channel `creator_id` 是 0，
    /// 直接下发会让【群主】标签在所有端都消失。
    #[test]
    fn owner_comes_from_the_roster_not_the_empty_creator_field() {
        let members = vec![member(7, MemberRole::Member), member(8, MemberRole::Owner)];
        assert_eq!(owner_id_of(&members, 0), 8);
    }

    /// 没有 Owner 行（异常数据）时才回落 creator_id，不能凭空变成 0。
    #[test]
    fn owner_falls_back_to_the_creator_when_the_roster_has_none() {
        let members = vec![member(7, MemberRole::Member)];
        assert_eq!(owner_id_of(&members, 42), 42);
    }

    /// 管理员列表有界且不含群主（群主走 owner_id，避免两处表达同一件事）。
    #[test]
    fn admins_exclude_the_owner() {
        let members = vec![
            member(7, MemberRole::Owner),
            member(8, MemberRole::Admin),
            member(9, MemberRole::Member),
            member(10, MemberRole::Admin),
        ];
        assert_eq!(admin_ids(&members), vec![8, 10]);
    }
}
