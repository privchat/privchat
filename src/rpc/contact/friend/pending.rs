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
use privchat_protocol::rpc::account::search::SearchedUser;
use privchat_protocol::rpc::contact::friend::{FriendPendingItem, FriendPendingResponse};
use serde_json::Value;

/// 处理 待处理好友申请列表 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理待处理好友申请列表请求: {:?}", body);

    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    match services.friend_service.get_pending_requests(user_id).await {
        Ok(requests) => {
            let mut items = Vec::with_capacity(requests.len());

            for req in requests {
                // 获取申请者用户资料
                let user = match helpers::get_user_profile_with_fallback(
                    req.from_user_id,
                    &services.user_repository,
                    &services.cache_manager,
                )
                .await
                {
                    Ok(Some(profile)) => SearchedUser {
                        user_id: profile.user_id.parse().unwrap_or(0),
                        username: profile.username,
                        nickname: profile.nickname,
                        avatar_url: profile.avatar_url,
                        user_type: profile.user_type,
                        search_session_id: 0,
                        is_friend: false,
                        can_send_message: false,
                    },
                    _ => SearchedUser {
                        user_id: req.from_user_id,
                        username: format!("user{}", req.from_user_id),
                        nickname: String::new(),
                        avatar_url: None,
                        user_type: 0,
                        search_session_id: 0,
                        is_friend: false,
                        can_send_message: false,
                    },
                };

                items.push(FriendPendingItem {
                    from_user_id: req.from_user_id,
                    user,
                    message: req.message,
                    created_at: req.created_at.timestamp_millis().max(0) as u64,
                });
            }

            let total = items.len();
            let response = FriendPendingResponse {
                requests: items,
                total,
            };

            tracing::debug!(
                "✅ 获取待处理好友申请列表成功: {} 有 {} 个待处理申请",
                user_id,
                total
            );
            Ok(serde_json::to_value(response)
                .map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))?)
        }
        Err(e) => {
            tracing::error!("❌ 获取待处理好友申请列表失败: {}", e);
            Err(RpcError::internal(format!(
                "获取待处理好友申请列表失败: {}",
                e
            )))
        }
    }
}
