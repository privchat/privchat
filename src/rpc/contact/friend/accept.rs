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
use crate::rpc::RpcServiceContext;
use crate::service::friend_service::AcceptFriendRequestResult;
use crate::service::EntityInvalidationPublisher;
use privchat_protocol::rpc::contact::friend::FriendAcceptRequest;
use privchat_protocol::EntityMutationHint;
use privchat_protocol::ErrorCode;
use serde_json::{json, Value};

/// 查 user 的显示名（display_name → username → fallback uid 字符串），作系统消息 refs 兜底快照。
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

    // F-sync.1: 同步状态变化到双方所有设备。requester=from_user_id, target=user_id
    // （注意命名：accept handler 里 `user_id` 是接受者=申请的 target，
    // `from_user_id` 是 requester）。new_status=1 (Accepted)。
    if !already_friends {
        push_helpers::push_friend_request_status_changed(
            &services,
            from_user_id, // requester
            user_id,      // target / actor
            1,            // Accepted
            user_id,
        )
        .await;

        // 成为好友系统消息——按 SYSTEM_MESSAGE_SPEC §5：
        //   template = "system.friend_request_accepted"
        //   refs = [{user, 同意者}, {user, 申请者}]
        // 文案本地化由各端 i18n 负责，refs[i].text 是兜底显示名快照。
        let accepter_name = resolve_display_name(&services, user_id).await;
        let requester_name = resolve_display_name(&services, from_user_id).await;
        let sys_payload = json!({
            "message_type": "system",
            "template": "system.friend_request_accepted",
            "refs": [
                {
                    "type": "user",
                    "target_id": user_id.to_string(),
                    "text": accepter_name,
                },
                {
                    "type": "user",
                    "target_id": from_user_id.to_string(),
                    "text": requester_name,
                },
            ],
        });
        if let Err(e) = services
            .message_service
            .send_direct_system_message(
                channel_id,
                vec![user_id, from_user_id],
                sys_payload.to_string(),
                json!({
                    "event": "friend.request.accepted",
                    "actor_id": user_id,
                    "target_ids": [from_user_id],
                }),
            )
            .await
        {
            tracing::warn!("⚠️ 写入成为好友系统消息失败 channel_id={}: {}", channel_id, e);
        }
    }
    let publisher = EntityInvalidationPublisher::new(services.connection_manager.clone());
    if let Err(error) = publisher
        .publish_friend_pair_change(user_id, from_user_id, EntityMutationHint::Upsert)
        .await
    {
        tracing::warn!(user_id, from_user_id, %error, "friend invalidation failed");
    }

    // 返回会话 ID
    Ok(json!(channel_id))
}
