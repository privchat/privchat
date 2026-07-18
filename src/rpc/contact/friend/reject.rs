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
use crate::service::EntityInvalidationPublisher;
use privchat_protocol::rpc::contact::friend::FriendRejectRequest;
use privchat_protocol::EntityMutationHint;
use serde_json::{json, Value};

/// 处理 拒绝好友申请 请求。
///
/// **F-sync.1 语义**：把 pending row（status=0）改成 Rejected(3)。
/// 不再物理 DELETE—— rejected 态需要通过 friendships.sync_version + entity sync
/// 分发到双方所有设备（requester 的 Sent 列表能看到"已拒绝"状态）。
///
/// 触发 status_changed push 给双方所有设备（在线唤醒），离线补偿由 entity sync 兜底。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 拒绝好友申请 请求: {:?}", body);

    let mut request: FriendRejectRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request payload: {}", e)))?;

    request.target_user_id = crate::rpc::get_current_user_id(&ctx)?;

    let from_user_id = request.from_user_id;
    let user_id = request.target_user_id;

    match services
        .friend_service
        .reject_friend_request(user_id, from_user_id)
        .await
    {
        Ok(changed) => {
            if changed {
                tracing::debug!(
                    "🛑 已拒绝好友申请: from={} target={}",
                    from_user_id,
                    user_id
                );
                // status_changed → requester + target 所有设备
                push_helpers::push_friend_request_status_changed(
                    &services,
                    from_user_id, // requester
                    user_id,      // target / actor
                    3,            // Rejected
                    user_id,
                )
                .await;
                let publisher =
                    EntityInvalidationPublisher::new(services.connection_manager.clone());
                if let Err(error) = publisher
                    .publish_friend_pair_change(from_user_id, user_id, EntityMutationHint::Delete)
                    .await
                {
                    tracing::warn!(from_user_id, user_id, %error, "friend invalidation failed");
                }
            } else {
                tracing::debug!(
                    "ℹ️ 拒绝好友申请：无 pending 行（已过期/已处理/已撤回）from={} target={}",
                    from_user_id,
                    user_id
                );
            }
            // FriendRejectResponse = bool。即使 affected=0 也算成功——前端意图
            //（让对方从 pending 列表消失）已达成。
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 拒绝好友申请失败: target={} from={} err={}",
                user_id,
                from_user_id,
                e
            );
            Err(RpcError::internal(format!(
                "Reject friend request failed: {}",
                e
            )))
        }
    }
}
