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

use crate::model::privacy::UserDetailSource;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use privchat_protocol::rpc::account::user::{AccountUserDetailRequest, DetailSourceType};
use serde_json::{json, Value};

/// 处理 获取用户详情 请求
///
/// 通过 user_id 获取用户完整信息
/// 必须提供来源（source）和来源ID（source_id）进行权限验证
/// 来源类型定义见 `privchat_protocol::rpc::account::user::DetailSourceType`
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理获取用户详情请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: AccountUserDetailRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id（搜索者）
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let searcher_id = request.user_id;
    let user_id = request.target_user_id;
    let source_str = &request.source;
    let source_id = &request.source_id;

    // 使用协议层枚举解析来源类型
    let source_type = DetailSourceType::from_str(source_str).ok_or_else(|| {
        RpcError::validation(format!(
            "Invalid source type: {}. Must be one of: search, group, friend, card_share, friend_pending, conversation",
            source_str
        ))
    })?;

    // 构建来源对象
    let source = match source_type {
        DetailSourceType::Search => {
            let search_session_id = source_id.parse::<u64>().map_err(|_| {
                RpcError::validation(format!("Invalid search_session_id: {}", source_id))
            })?;
            UserDetailSource::Search { search_session_id }
        }
        DetailSourceType::Group => {
            let group_id = source_id
                .parse::<u64>()
                .map_err(|_| RpcError::validation(format!("Invalid group_id: {}", source_id)))?;
            UserDetailSource::Group { group_id }
        }
        DetailSourceType::Friend => {
            let friend_id = source_id
                .parse::<u64>()
                .map_err(|_| RpcError::validation(format!("Invalid friend_id: {}", source_id)))?;
            UserDetailSource::Friend {
                friend_id: Some(friend_id),
            }
        }
        DetailSourceType::CardShare => {
            let share_id = source_id
                .parse::<u64>()
                .map_err(|_| RpcError::validation(format!("Invalid share_id: {}", source_id)))?;
            UserDetailSource::CardShare { share_id }
        }
        DetailSourceType::FriendPending => {
            UserDetailSource::Friend { friend_id: None }
        }
        DetailSourceType::Conversation => {
            let channel_id = source_id
                .parse::<u64>()
                .map_err(|_| RpcError::validation(format!("Invalid channel_id: {}", source_id)))?;
            UserDetailSource::Conversation { channel_id }
        }
    };

    // 验证访问权限
    match services
        .privacy_service
        .validate_detail_access(searcher_id, user_id, source)
        .await
    {
        Ok(_) => {
            // 权限验证通过，从数据库读取用户资料
            match helpers::get_user_profile_with_fallback(
                user_id,
                &services.user_repository,
                &services.cache_manager,
            )
            .await
            {
                Ok(Some(user_profile)) => {
                    // ✨ 检查好友关系和发消息权限
                    let is_friend = services
                        .friend_service
                        .is_friend(searcher_id, user_id)
                        .await;

                    // 检查是否可以发消息
                    let can_send_message = {
                        // 如果是好友，可以发消息
                        if is_friend {
                            true
                        } else {
                            // 不是好友，检查黑名单和隐私设置
                            let (sender_blocks_receiver, receiver_blocks_sender) = services
                                .blacklist_service
                                .check_mutual_block(searcher_id, user_id)
                                .await
                                .unwrap_or((false, false));

                            // 如果被拉黑，不能发消息
                            if receiver_blocks_sender || sender_blocks_receiver {
                                false
                            } else {
                                // 检查隐私设置：是否允许接收非好友消息
                                match services
                                    .privacy_service
                                    .get_or_create_privacy_settings(user_id)
                                    .await
                                {
                                    Ok(privacy_settings) => {
                                        privacy_settings.allow_receive_message_from_non_friend
                                    }
                                    Err(_) => true, // 默认允许
                                }
                            }
                        }
                    };

                    let uid: u64 = user_profile.user_id.parse().unwrap_or(user_id);
                    Ok(json!({
                        "user_id": uid,
                        "username": user_profile.username, // 账号
                        "nickname": user_profile.nickname, // 昵称
                        "avatar_url": user_profile.avatar_url, // 头像
                        "phone": user_profile.phone, // 手机号（可选）
                        "email": user_profile.email, // 邮箱（可选）
                        "user_type": user_profile.user_type, // 用户类型（0 普通 1 系统 2 机器人）
                        "is_friend": is_friend, // ✨ 是否好友
                        "can_send_message": can_send_message, // ✨ 是否有权限发消息
                        "source_type": source_str, // 本次查看的来源类型
                        "source_id": source_id, // 本次查看的来源 ID
                    }))
                }
                Ok(None) => Err(RpcError::not_found(format!("User '{}' not found", user_id))),
                Err(e) => {
                    tracing::error!("Failed to get user profile: {}", e);
                    Err(RpcError::internal("Database error".to_string()))
                }
            }
        }
        Err(e) => {
            tracing::warn!("❌ 权限验证失败: {} -> {}: {}", searcher_id, user_id, e);
            Err(RpcError::forbidden(format!("Access denied: {}", e)))
        }
    }
}
