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
use crate::service::friend_service::AcceptFriendRequestResult;
use privchat_protocol::rpc::contact::friend::FriendAcceptRequest;
use privchat_protocol::ErrorCode;
use serde_json::{json, Value};

/// 处理 接受好友申请 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 接受好友申请 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendAcceptRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request payload: {}", e)))?;

    // 从 ctx 填充 target_user_id
    request.target_user_id = crate::rpc::get_current_user_id(&ctx)?;

    let from_user_id = request.from_user_id;
    let user_id = request.target_user_id;

    // 先原子化处理好友申请，避免"预检查通过但后续已被并发消费"导致误判过期
    let already_friends = match services
        .friend_service
        .accept_friend_request_with_source(user_id, from_user_id)
        .await
    {
        Ok(AcceptFriendRequestResult::Accepted(_)) => false,
        Ok(AcceptFriendRequestResult::AlreadyFriends) => true,
        Err(crate::error::ServerError::NotFound(_)) => {
            return Err(RpcError::from_code(
                ErrorCode::FriendRequestExpired,
                ErrorCode::FriendRequestExpired.message().to_string(),
            ));
        }
        Err(e) => {
            tracing::error!(
                "❌ 接受好友申请失败: user_id={}, from_user_id={}, err={}",
                user_id,
                from_user_id,
                e
            );
            return Err(RpcError::internal(format!(
                "Accept friend request failed: {}",
                e
            )));
        }
    };

    // 所有 Direct 会话创建统一走 channel_service.get_or_create_direct_channel：
    // 内部做 smaller/larger 规范化 + advisory lock + ON CONFLICT 兜底，
    // 无需外层事务。
    let (channel_id, _created) = services
        .channel_service
        .get_or_create_direct_channel(user_id, from_user_id, None, None)
        .await
        .map_err(|e| {
            tracing::error!("❌ 创建或获取私聊会话失败: {}", e);
            RpcError::internal(format!(
                "Accept friend request failed: cannot create channel - {}",
                e
            ))
        })?;

    tracing::debug!("✅ 私聊会话就绪: channel_id={}", channel_id);

    if already_friends {
        tracing::debug!(
            "ℹ️ 接受请求时检测到已是好友: {} <-> {}",
            user_id,
            from_user_id
        );
    }

    tracing::debug!(
        "✅ 好友申请接受成功: {} <-> {}, channel_id: {}",
        user_id,
        from_user_id,
        channel_id
    );

    // 返回会话 ID
    Ok(json!(channel_id))
}
