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
use crate::rpc::{helpers, RpcServiceContext};
use privchat_protocol::rpc::group::member::GroupMemberListRequest;
use serde_json::{json, Value};

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
            // 获取成员详细信息（从数据库读取）
            let mut member_list = Vec::new();
            for member in members {
                if let Ok(Some(profile)) = helpers::get_user_profile_with_fallback(
                    member.user_id,
                    &services.user_repository,
                    &services.cache_manager,
                )
                .await
                {
                    member_list.push(json!({
                        "user_id": member.user_id,
                        "username": profile.username, // 账号
                        "nickname": profile.nickname, // 昵称
                        "avatar_url": profile.avatar_url, // 头像
                        "role": format!("{:?}", member.role),
                        "joined_at": member.joined_at.timestamp_millis(),
                        "is_muted": member.is_muted,
                    }));
                }
            }

            tracing::debug!(
                "✅ 获取群成员列表成功: {} 有 {} 个成员",
                group_id,
                member_list.len()
            );
            Ok(json!({
                "members": member_list,
                "total": member_list.len(),
            }))
        }
        Err(e) => {
            tracing::error!("❌ 获取群成员列表失败: {}", e);
            Err(RpcError::internal(format!("获取群成员列表失败: {}", e)))
        }
    }
}
