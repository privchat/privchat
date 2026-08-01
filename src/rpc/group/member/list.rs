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
use privchat_protocol::rpc::group::member::{
    GroupMemberInfo, GroupMemberListRequest, GroupMemberListResponse,
};
use serde_json::Value;
use std::collections::HashMap;

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn resolve_display_name(
    alias: Option<&str>,
    nickname: Option<&str>,
    visible_username: Option<&str>,
    user_id: u64,
) -> String {
    non_blank(alias)
        .or_else(|| non_blank(nickname))
        .or_else(|| non_blank(visible_username))
        .map(str::to_owned)
        .unwrap_or_else(|| user_id.to_string())
}

/// 处理 群成员列表 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 群成员列表 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupMemberListRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;

    // 获取群成员列表
    match services
        .channel_service
        .get_channel_members(&group_id)
        .await
    {
        Ok(members) => {
            let user_ids = members
                .iter()
                .map(|member| member.user_id)
                .collect::<Vec<_>>();
            let profiles = services
                .user_repository
                .find_group_member_projections(&user_ids)
                .await
                .map_err(|e| RpcError::internal(format!("批量读取群成员资料失败: {e}")))?;
            let profiles = profiles
                .into_iter()
                .map(|profile| (profile.user_id as u64, profile))
                .collect::<HashMap<_, _>>();

            // Always preserve roster cardinality. A missing user projection is
            // represented by the privacy-safe uid fallback, not by dropping the member.
            let member_list = members
                .into_iter()
                .map(|member| {
                    let profile = profiles.get(&member.user_id);
                    let alias = non_blank(member.display_name.as_deref()).map(str::to_owned);
                    let nickname = profile
                        .and_then(|profile| non_blank(profile.display_name.as_deref()))
                        .unwrap_or_default()
                        .to_owned();
                    // PROFILE_VISIBILITY: username remains visible only to the user themself.
                    let username = if member.user_id == request.user_id {
                        profile
                            .and_then(|profile| non_blank(profile.username.as_deref()))
                            .unwrap_or_default()
                            .to_owned()
                    } else {
                        String::new()
                    };
                    GroupMemberInfo {
                        user_id: member.user_id,
                        display_name: resolve_display_name(
                            alias.as_deref(),
                            Some(&nickname),
                            Some(&username),
                            member.user_id,
                        ),
                        alias,
                        username,
                        nickname,
                        avatar_url: profile.and_then(|profile| profile.avatar_url.clone()),
                        user_type: profile.map(|profile| profile.user_type).unwrap_or_default(),
                        // Stable lowercase role contract for every client permission gate.
                        role: format!("{:?}", member.role).to_lowercase(),
                        joined_at: member.joined_at.timestamp_millis().max(0) as u64,
                        is_muted: member.is_muted,
                    }
                })
                .collect::<Vec<_>>();

            tracing::debug!(
                "✅ 获取群成员列表成功: {} 有 {} 个成员",
                group_id,
                member_list.len()
            );
            serde_json::to_value(GroupMemberListResponse {
                total: member_list.len(),
                members: member_list,
            })
            .map_err(|e| RpcError::internal(format!("序列化群成员列表失败: {e}")))
        }
        Err(e) => {
            tracing::error!("❌ 获取群成员列表失败: {}", e);
            Err(RpcError::internal(format!("获取群成员列表失败: {}", e)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_display_name;

    #[test]
    fn display_name_uses_the_frozen_priority_without_exposing_hidden_username() {
        assert_eq!(
            resolve_display_name(Some(" group alias "), Some("nickname"), Some("username"), 7),
            "group alias"
        );
        assert_eq!(
            resolve_display_name(None, Some("nickname"), None, 7),
            "nickname"
        );
        assert_eq!(
            resolve_display_name(None, None, Some("username"), 7),
            "username"
        );
        assert_eq!(resolve_display_name(None, None, None, 7), "7");
    }
}
