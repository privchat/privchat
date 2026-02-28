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
use privchat_protocol::rpc::GroupMemberAddRequest;
use serde_json::{json, Value};

/// 处理 添加群成员 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    // ✅ 使用 protocol 定义自动反序列化
    let request: GroupMemberAddRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    // ✅ 从 ctx 获取 inviter_id（安全性）
    // inviter_id 是执行邀请操作的人，必须从 ctx 获取
    let inviter_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let user_id = request.user_id; // 被邀请的用户（从请求体）
    let role = request.role.as_deref().unwrap_or("member");

    tracing::debug!(
        "🔧 添加群组成员: {} 邀请 {} 加入群组 {} (角色: {})",
        inviter_id,
        user_id,
        group_id,
        role
    );

    // 确定成员角色
    let member_role = match role.to_lowercase().as_str() {
        "admin" => crate::model::channel::MemberRole::Admin,
        "owner" => crate::model::channel::MemberRole::Owner,
        _ => crate::model::channel::MemberRole::Member,
    };

    // ✨ 先添加到数据库的参与者表（通过 channel_service）
    if let Err(e) = services
        .channel_service
        .add_participant(group_id, user_id, member_role)
        .await
    {
        tracing::warn!("⚠️ 添加参与者到数据库失败: {}，继续添加到频道", e);
    }

    // 添加成员到群组频道（内存）
    match services
        .channel_service
        .add_member_to_group(group_id, user_id)
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 成功添加成员 {} 到群组 {}", user_id, group_id);

            // ✨ 如果指定了 admin 或 owner 角色，设置成员角色
            let channel_role = match member_role {
                crate::model::channel::MemberRole::Owner => {
                    crate::model::channel::MemberRole::Owner
                }
                crate::model::channel::MemberRole::Admin => {
                    crate::model::channel::MemberRole::Admin
                }
                crate::model::channel::MemberRole::Member => {
                    crate::model::channel::MemberRole::Member
                }
            };

            if member_role != crate::model::channel::MemberRole::Member {
                match services
                    .channel_service
                    .set_member_role(&group_id, &user_id, channel_role)
                    .await
                {
                    Ok(_) => {
                        tracing::debug!("✅ 成功设置成员 {} 角色为 {:?}", user_id, role);
                    }
                    Err(e) => {
                        tracing::warn!(
                            "⚠️ 设置成员 {} 角色失败: {}，成员已添加但角色为普通成员",
                            user_id,
                            e
                        );
                    }
                }
            }

            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!("❌ 添加群组成员失败: {}", e);
            Err(RpcError::internal(format!("添加群组成员失败: {}", e)))
        }
    }
}
