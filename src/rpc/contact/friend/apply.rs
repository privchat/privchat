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
use privchat_protocol::rpc::contact::friend::FriendApplyRequest;
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
            _ => {
                return Err(RpcError::validation(format!(
                    "Invalid source type: {}. Must be one of: search, group, card_share, friend",
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
