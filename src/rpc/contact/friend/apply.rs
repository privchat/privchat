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

use crate::rpc::contact::friend::push_helpers;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use crate::service::EntityInvalidationPublisher;
use privchat_protocol::rpc::contact::friend::FriendApplyRequest;
use privchat_protocol::EntityMutationHint;
use serde_json::{json, Value};

/// 处理 好友申请 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 好友申请 请求: {:?}", body);

    // 在 body 被 move 之前提取额外的字段（用于兼容性）
    let has_qrcode = body.get("qrcode").is_some();
    let qrcode = body
        .get("qrcode")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let phone = body
        .get("phone")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendApplyRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 from_user_id
    request.from_user_id = crate::rpc::get_current_user_id(&ctx)?;

    let from_user_id = request.from_user_id;
    let target_user_id = request.target_user_id;
    let message = request.message.as_deref().unwrap_or("");

    // 解析来源信息
    let (source_str, source_id_str) = (request.source.as_deref(), request.source_id.as_deref());

    tracing::debug!(
        "🔍 [DEBUG] source_str={:?}, source_id_str={:?}",
        source_str,
        source_id_str
    );

    // 构建 UserDetailSource 用于验证（和 detail 接口使用相同的验证逻辑）
    let detail_source = if let (Some(source_str), Some(source_id_str)) = (source_str, source_id_str)
    {
        tracing::debug!(
            "✅ [DEBUG] 进入 if let 分支: source={}, source_id={}",
            source_str,
            source_id_str
        );
        Some(match source_str {
            "search" => {
                let search_session_id = source_id_str.parse::<u64>().map_err(|_| {
                    RpcError::validation(format!("Invalid search_session_id: {}", source_id_str))
                })?;
                crate::model::privacy::UserDetailSource::Search { search_session_id }
            }
            "group" => {
                let group_id = source_id_str.parse::<u64>().map_err(|_| {
                    RpcError::validation(format!("Invalid group_id: {}", source_id_str))
                })?;
                crate::model::privacy::UserDetailSource::Group { group_id }
            }
            "card_share" => {
                let share_id = source_id_str.parse::<u64>().map_err(|_| {
                    RpcError::validation(format!("Invalid share_id: {}", source_id_str))
                })?;
                crate::model::privacy::UserDetailSource::CardShare { share_id }
            }
            "friend" => {
                let friend_id = source_id_str.parse::<u64>().map_err(|_| {
                    RpcError::validation(format!("Invalid friend_id: {}", source_id_str))
                })?;
                crate::model::privacy::UserDetailSource::Friend {
                    friend_id: Some(friend_id),
                }
            }
            "conversation" => {
                // 在 1v1 / 群聊里点开对方资料 → 添加好友。
                // source_id = 来源 channel_id（server 校验 from_user_id 与对方在该 channel 共存）。
                let channel_id = source_id_str.parse::<u64>().map_err(|_| {
                    RpcError::validation(format!("Invalid channel_id: {}", source_id_str))
                })?;
                crate::model::privacy::UserDetailSource::Conversation { channel_id }
            }
            _ => {
                return Err(RpcError::validation(format!(
                    "Invalid source type: {}. Must be one of: search, group, card_share, friend, conversation",
                    source_str
                )));
            }
        })
    } else {
        // 如果没有提供来源，尝试使用 qrcode 或 phone（这些不需要 source_id）
        if has_qrcode {
            // qrcode 来源不需要验证（扫码本身就是验证）
            None
        } else {
            return Err(RpcError::validation(
                "source and source_id are required (same as detail interface)".to_string(),
            ));
        }
    };

    // 验证来源（和 detail 接口使用相同的验证逻辑）
    if let Some(detail_source) = detail_source {
        match services
            .privacy_service
            .validate_detail_access(from_user_id, target_user_id, detail_source.clone())
            .await
        {
            Ok(_) => {
                tracing::debug!("✅ 来源验证通过: {} -> {}", from_user_id, target_user_id);
            }
            Err(e) => {
                tracing::warn!(
                    "❌ 来源验证失败: {} -> {}: {}",
                    from_user_id,
                    target_user_id,
                    e
                );
                return Err(RpcError::forbidden(format!(
                    "Source validation failed: {}",
                    e
                )));
            }
        }
    }

    // 群业务策略强制：`privchat_groups.allow_member_add_friend = false` 时，禁止**经由该群**
    // 发起好友申请。覆盖两条来源：source=group（群成员列表点开）与 source=conversation
    // （群聊会话里点开——source_id 是 channel_id，群聊 channel_id == group_id；DM 查无
    // group policy 返回 None 自然放行）。仅有开关无强制曾是纯摆设（客户端照样能加）。
    if matches!(source_str, Some("group") | Some("conversation")) {
        if let Some(gid) = source_id_str.and_then(|s| s.parse::<u64>().ok()) {
            if let Ok(Some(policy)) = services.channel_service.get_group_policy(gid).await {
                if !policy.allow_member_add_friend {
                    tracing::info!(
                        "🚫 群 {} 禁止成员互加好友，拒绝申请: {} -> {}",
                        gid,
                        from_user_id,
                        target_user_id
                    );
                    return Err(RpcError::from_code(
                        privchat_protocol::ErrorCode::GroupAddFriendDisabled,
                        "该群不允许成员互相添加好友".to_string(),
                    ));
                }
            }
        }
    }

    // 构建 FriendRequestSource（用于存储）
    let source: Option<crate::model::privacy::FriendRequestSource> =
        if let (Some(source_str), Some(source_id_str)) = (source_str, source_id_str) {
            match source_str {
                "search" => {
                    let search_session_id = source_id_str.parse::<u64>().unwrap_or(0);
                    Some(crate::model::privacy::FriendRequestSource::Search { search_session_id })
                }
                "group" => {
                    let group_id = source_id_str.parse::<u64>().unwrap_or(0);
                    Some(crate::model::privacy::FriendRequestSource::Group { group_id })
                }
                "card_share" => {
                    let share_id = source_id_str.parse::<u64>().unwrap_or(0);
                    Some(crate::model::privacy::FriendRequestSource::CardShare { share_id })
                }
                "friend" => {
                    // friend 来源在 FriendRequestSource 中没有对应项，使用 search 作为占位
                    let search_session_id = source_id_str.parse::<u64>().unwrap_or(0);
                    Some(crate::model::privacy::FriendRequestSource::Search { search_session_id })
                }
                "conversation" => {
                    let channel_id = source_id_str.parse::<u64>().unwrap_or(0);
                    Some(crate::model::privacy::FriendRequestSource::Conversation { channel_id })
                }
                _ => {
                    tracing::warn!("⚠️ 未知的来源类型: {}, 忽略来源信息", source_str);
                    None
                }
            }
        } else if let Some(qrcode_str) = qrcode {
            Some(crate::model::privacy::FriendRequestSource::Qrcode { qrcode: qrcode_str })
        } else if let Some(phone_str) = phone {
            Some(crate::model::privacy::FriendRequestSource::Phone { phone: phone_str })
        } else {
            None
        };

    // 如果来源是名片分享，标记分享记录为已使用
    if let Some(crate::model::privacy::FriendRequestSource::CardShare { share_id }) = &source {
        if let Err(e) = services
            .cache_manager
            .mark_card_share_as_used(*share_id, from_user_id)
            .await
        {
            tracing::warn!("⚠️ 标记名片分享为已使用失败: {}", e);
            // 不阻止好友申请，只记录警告
        }
    }

    // 检查目标用户是否存在（从数据库读取）
    match helpers::get_user_profile_with_fallback(
        target_user_id,
        &services.user_repository,
        &services.cache_manager,
    )
    .await
    {
        Ok(Some(target_user)) => {
            // 发送好友请求（带来源）
            match services
                .friend_service
                .send_friend_request_with_source(
                    from_user_id,
                    target_user_id,
                    if message.is_empty() {
                        None
                    } else {
                        Some(message.to_string())
                    },
                    source,
                )
                .await
            {
                Ok(_) => {
                    tracing::debug!(
                        "✅ 好友申请已发送: {} -> {} ({})",
                        from_user_id,
                        target_user_id,
                        message
                    );

                    // 双路 push（USER_INBOX_EVENT_ENVELOPE_SPEC + F-sync.1）：
                    // 1. friend.request.received → target 所有设备；
                    // 2. friend.request.sent → requester 自己所有设备（多端同步）。
                    //
                    // 仅在线唤醒，离线补偿由 friendships 表 + entity sync 兜底。
                    push_helpers::push_friend_request_received(
                        &services,
                        from_user_id,
                        target_user_id,
                        message,
                    )
                    .await;
                    push_helpers::push_friend_request_sent(&services, from_user_id, target_user_id)
                        .await;
                    let publisher =
                        EntityInvalidationPublisher::new(services.connection_manager.clone());
                    if let Err(error) = publisher
                        .publish_friend_pair_change(
                            from_user_id,
                            target_user_id,
                            EntityMutationHint::Upsert,
                        )
                        .await
                    {
                        tracing::warn!(from_user_id, target_user_id, %error, "friend invalidation failed");
                    }

                    Ok(json!({
                        // 协议约定 user_id 必须是 u64 数字，不是字符串
                        "user_id": target_user_id,
                        "username": target_user.username,
                        "status": "pending",
                        "added_at": chrono::Utc::now().timestamp_millis(),
                        "message": message
                    }))
                }
                Err(e) => {
                    tracing::error!("❌ 发送好友申请失败: {}", e);
                    Err(RpcError::internal(format!("发送好友申请失败: {}", e)))
                }
            }
        }
        Ok(None) => Err(RpcError::not_found(format!(
            "User '{}' not found",
            target_user_id
        ))),
        Err(e) => {
            tracing::error!("Failed to get user profile: {}", e);
            Err(RpcError::internal("Database error".to_string()))
        }
    }
}
