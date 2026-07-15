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
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::GroupMemberAddRequest;
use serde_json::{json, Value};

const USER_TYPE_SYSTEM: i16 = 1;

/// 查 user 的显示名（display_name → username → fallback uid 字符串）
async fn resolve_display_name(services: &RpcServiceContext, user_id: u64) -> String {
    services
        .user_service
        .find_by_id(user_id)
        .await
        .ok()
        .flatten()
        .and_then(|u| u.display_name.or(u.username))
        .unwrap_or_else(|| user_id.to_string())
}

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

    // System User (user_type=1) 禁止入群——spec 07-application/SYSTEM_USER_SPEC §4
    // + 02-server/CHANNEL_SPEC §10.5。
    match helpers::lookup_user_type(user_id, &services.user_repository, &services.cache_manager)
        .await
    {
        Ok(Some(t)) if t == USER_TYPE_SYSTEM => {
            return Err(RpcError::from_code(
                ErrorCode::SystemUserNotGroupInvitable,
                format!("user {} is a system user and cannot join a group", user_id),
            ));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!("校验被邀请人 user_type 失败 user_id={}: {}", user_id, e);
            return Err(RpcError::internal("校验成员身份失败".to_string()));
        }
    }

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

            // 入群系统消息——按 SYSTEM_MESSAGE_SPEC §5：
            //   - inviter == 0 / inviter == invitee：template = "system.member_joined"  refs = [{user, 加入者}]
            //   - inviter != invitee：               template = "system.member_invited" refs = [{user, 邀请者}, {user, 加入者}]
            // 文案本地化由各端 i18n 负责，refs[i].text 是兜底显示名快照。
            let invitee_name = resolve_display_name(&services, user_id).await;
            let sys_payload = if inviter_id == 0 || inviter_id == user_id {
                json!({
                    "message_type": "system",
                    "template": "system.member_joined",
                    "refs": [{
                        "type": "user",
                        "target_id": user_id.to_string(),
                        "text": invitee_name,
                    }],
                })
            } else {
                let inviter_name = resolve_display_name(&services, inviter_id).await;
                json!({
                    "message_type": "system",
                    "template": "system.member_invited",
                    "refs": [
                        {
                            "type": "user",
                            "target_id": inviter_id.to_string(),
                            "text": inviter_name,
                        },
                        {
                            "type": "user",
                            "target_id": user_id.to_string(),
                            "text": invitee_name,
                        },
                    ],
                })
            };
            let sys_content = sys_payload.to_string();
            if let Err(e) = services
                .message_service
                .send_group_system_message(group_id, sys_content, json!({}))
                .await
            {
                tracing::warn!("⚠️ 写入入群系统消息失败 group_id={}: {}", group_id, e);
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
