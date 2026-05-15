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
use privchat_protocol::rpc::contact::friend::FriendRecallRequest;
use serde_json::{json, Value};

/// 处理 撤回好友申请 请求（requester 主动撤回自己发出的、尚未处理的 pending）。
///
/// **F-sync.1 语义**：把 (user_id=me, friend_id=target, status=0) 改成
/// Recalled(4)。row 保留，让 requester 的 Sent 列表能继续看到"已撤回"状态，
/// 同时通过 friendships.sync_version + entity sync 把这个状态分发到双方所有
/// 设备——target 的 Received 列表也会同步消失（client 按 status != 0 过滤）。
///
/// 重新申请：requester 可以再次调 friend/apply（ON CONFLICT 把 status 改回 0）。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 撤回好友申请 请求: {:?}", body);

    let mut request: FriendRecallRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request payload: {}", e)))?;

    request.from_user_id = crate::rpc::get_current_user_id(&ctx)?;

    let requester_id = request.from_user_id;
    let target_user_id = request.target_user_id;

    match services
        .friend_service
        .recall_friend_request(requester_id, target_user_id)
        .await
    {
        Ok(changed) => {
            if changed {
                tracing::debug!(
                    "↩️ 已撤回好友申请: requester={} target={}",
                    requester_id,
                    target_user_id
                );
                push_helpers::push_friend_request_status_changed(
                    &services,
                    requester_id,
                    target_user_id,
                    4, // Recalled
                    requester_id,
                )
                .await;
            } else {
                tracing::debug!(
                    "ℹ️ 撤回好友申请：无 pending 行（已过期/已被处理）requester={} target={}",
                    requester_id,
                    target_user_id
                );
            }
            // FriendRecallResponse = bool。affected=0 仍返回 true——前端意图已达成
            // （如已被对方接受，UI 会在下次 entity sync 时看到 accepted 态）。
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 撤回好友申请失败: requester={} target={} err={}",
                requester_id,
                target_user_id,
                e
            );
            Err(RpcError::internal(format!(
                "Recall friend request failed: {}",
                e
            )))
        }
    }
}
