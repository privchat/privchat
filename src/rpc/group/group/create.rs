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
use privchat_protocol::rpc::group::group::GroupCreateRequest;
use serde_json::{json, Value};

const USER_TYPE_SYSTEM: i16 = 1;

/// 处理 创建群组 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 创建群组 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupCreateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 creator_id
    request.creator_id = crate::rpc::get_current_user_id(&ctx)?;

    let name = &request.name;
    let description = request.description.as_deref().unwrap_or("");
    let creator_id = request.creator_id;

    // System User (user_type=1) 禁止入群——spec 07-application/SYSTEM_USER_SPEC §4
    // + 02-server/CHANNEL_SPEC §10.5。这里前置校验 initial_members，避免后续
    // add_participant / add_member_to_group 写入再回滚。
    if let Some(initial_members) = request.member_ids.as_deref() {
        for &uid in initial_members {
            if uid == creator_id {
                continue;
            }
            match helpers::lookup_user_type(uid, &services.user_repository, &services.cache_manager)
                .await
            {
                Ok(Some(t)) if t == USER_TYPE_SYSTEM => {
                    return Err(RpcError::from_code(
                        ErrorCode::SystemUserNotGroupInvitable,
                        format!("user {} is a system user and cannot join a group", uid),
                    ));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("校验 initial_members user_type 失败 uid={}: {}", uid, e);
                    return Err(RpcError::internal("校验成员身份失败".to_string()));
                }
            }
        }
    }

    // 检查创建者是否存在（从数据库读取）
    match helpers::get_user_profile_with_fallback(
        creator_id,
        &services.user_repository,
        &services.cache_manager,
    )
    .await
    {
        Ok(Some(_)) => {
            // ✨ 确保用户存在于数据库中（用于外键约束）
            // 检查用户是否存在于数据库中
            let user_exists = services
                .user_repository
                .exists(creator_id)
                .await
                .unwrap_or(false);

            if !user_exists {
                tracing::warn!("⚠️ 用户 {} 不存在于数据库中，尝试创建", creator_id);
                // 如果用户不存在，尝试从缓存获取信息并创建用户
                if let Ok(Some(profile)) = services.cache_manager.get_user_profile(creator_id).await
                {
                    use crate::model::user::User;
                    let mut user = User::new(creator_id, profile.username.clone());
                    user.phone = profile.phone.clone();
                    user.email = profile.email.clone();
                    user.display_name = Some(profile.nickname.clone());
                    user.avatar_url = profile.avatar_url.clone();

                    if let Err(e) = services.user_repository.create(&user).await {
                        tracing::error!("❌ 创建用户失败: {}", e);
                        return Err(RpcError::internal(format!("无法创建用户: {}", e)));
                    }
                    tracing::debug!("✅ 用户已创建: {}", creator_id);
                } else {
                    return Err(RpcError::not_found(format!(
                        "用户 {} 不存在于数据库中，且无法从缓存获取",
                        creator_id
                    )));
                }
            }

            // ✨ 创建数据库会话（channel_service 会在内部创建群组记录）
            use crate::model::channel::{ChannelType, CreateChannelRequest};

            let create_request = CreateChannelRequest {
                channel_type: ChannelType::Group,
                name: Some(name.to_string()),
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                },
                member_ids: vec![], // 初始成员只有创建者，已在 Channel::new_group 中添加
                is_public: Some(false),
                max_members: None,
            };

            // 创建数据库会话（让数据库自动生成 channel_id）
            match services
                .channel_service
                .create_channel(creator_id, create_request)
                .await
            {
                Ok(response) => {
                    // ✨ 检查响应是否成功
                    if !response.success {
                        let error_msg =
                            response.error.unwrap_or_else(|| "创建会话失败".to_string());
                        tracing::error!("❌ 创建群聊会话失败: {}", error_msg);
                        return Err(RpcError::internal(format!(
                            "创建群聊会话失败: {}",
                            error_msg
                        )));
                    }

                    let actual_channel_id = response.channel.id;
                    if actual_channel_id == 0 {
                        tracing::error!("❌ 创建群聊会话失败: channel_id 为 0");
                        return Err(RpcError::internal(
                            "创建群聊会话失败: channel_id 为 0".to_string(),
                        ));
                    }

                    tracing::debug!("✅ 群聊会话已创建: {}", actual_channel_id);

                    // 使用 channel_id 创建 Channel（确保 Channel 和 Channel 使用相同的 ID）
                    match services
                        .channel_service
                        .create_group_chat_with_id(creator_id, name.to_string(), actual_channel_id)
                        .await
                    {
                        Ok(_) => {
                            tracing::debug!("✅ 群组创建成功: {} -> {}", name, actual_channel_id);
                        }
                        Err(e) => {
                            tracing::warn!("⚠️ 创建群组频道失败: {}，频道可能已存在", e);
                        }
                    }

                    // 把请求中的初始成员（除创建者自己外）逐个加进群——一次性批量加，
                    // 完成后发**一条**邀请系统消息（参考微信：无"X 创建了群聊"提示，直接
                    // "X 邀请 Y、Z 加入了群聊"）。
                    let initial_members: Vec<u64> = request
                        .member_ids
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .copied()
                        .filter(|uid| *uid != creator_id)
                        .collect();

                    for &uid in &initial_members {
                        if let Err(e) = services
                            .channel_service
                            .add_participant(
                                actual_channel_id,
                                uid,
                                crate::model::channel::MemberRole::Member,
                            )
                            .await
                        {
                            tracing::warn!("⚠️ 初始成员入库失败 uid={}: {}", uid, e);
                        }
                        if let Err(e) = services
                            .channel_service
                            .add_member_to_group(actual_channel_id, uid)
                            .await
                        {
                            tracing::warn!("⚠️ 初始成员加入群频道失败 uid={}: {}", uid, e);
                        }
                    }

                    if !initial_members.is_empty() {
                        // 构建系统消息 payload —— spec §3 + §4 + §5
                        // template = "system.member_invited"
                        // refs = [{user, 邀请者}, {user, 受邀者1}, {user, 受邀者2}, ...]
                        // 客户端用 `{0} 邀请 {1+} 加入了群聊` 模板 + `{1+}` 列表展开渲染。
                        async fn resolve_name(
                            services: &crate::rpc::RpcServiceContext,
                            uid: u64,
                        ) -> String {
                            services
                                .user_service
                                .find_by_id(uid)
                                .await
                                .ok()
                                .flatten()
                                .and_then(|u| u.display_name.or(u.username))
                                .unwrap_or_else(|| uid.to_string())
                        }

                        let inviter_name = resolve_name(&services, creator_id).await;
                        let mut refs: Vec<Value> = vec![json!({
                            "type": "user",
                            "target_id": creator_id.to_string(),
                            "text": inviter_name,
                        })];
                        for &uid in &initial_members {
                            let name = resolve_name(&services, uid).await;
                            refs.push(json!({
                                "type": "user",
                                "target_id": uid.to_string(),
                                "text": name,
                            }));
                        }

                        let sys_payload = json!({
                            "message_type": "system",
                            "template": "system.member_invited",
                            "refs": refs,
                        });
                        if let Err(e) = services
                            .message_service
                            .send_group_system_message(
                                actual_channel_id,
                                sys_payload.to_string(),
                                json!({}),
                            )
                            .await
                        {
                            tracing::warn!(
                                "⚠️ 写入入群系统消息失败 group_id={}: {}",
                                actual_channel_id,
                                e
                            );
                        }
                    }

                    // 返回客户端期望的群组信息格式（返回 channel_id，客户端应使用此 ID 发送消息）
                    let member_count = 1 + initial_members.len();
                    Ok(json!({
                        "group_id": actual_channel_id, // ✨ 返回会话 ID 给客户端
                        "name": name,
                        "description": description,
                        "member_count": member_count,
                        "created_at": chrono::Utc::now().timestamp_millis(),
                        "creator_id": creator_id
                    }))
                }
                Err(e) => {
                    tracing::error!("❌ 创建群聊会话失败: {}", e);
                    Err(RpcError::internal(format!("创建群聊会话失败: {}", e)))
                }
            }
        }
        Ok(None) => Err(RpcError::not_found(format!(
            "Creator '{}' not found",
            creator_id
        ))),
        Err(e) => {
            tracing::error!("Failed to get user profile: {}", e);
            Err(RpcError::internal("Database error".to_string()))
        }
    }
}
