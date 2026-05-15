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
use privchat_protocol::rpc::contact::friend::FriendRejectRequest;
use serde_json::{json, Value};

/// 处理 拒绝好友申请 请求。
///
/// 语义：从 friendships 表里删除 pending 行（status=0），让申请人可以重新发起
/// 申请。**不**通知申请人——v1 显式选择不引入"被拒"系统消息或推送，避免心理
/// 摩擦；申请人可通过自己的 sent_list（未来接口）自行发现申请已不在 pending。
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
        Ok(removed) => {
            if removed {
                tracing::debug!(
                    "🛑 已拒绝好友申请: from={} target={}",
                    from_user_id,
                    user_id
                );
            } else {
                tracing::debug!(
                    "ℹ️ 拒绝好友申请：无 pending 行（已过期/已处理）from={} target={}",
                    from_user_id,
                    user_id
                );
            }
            // FriendRejectResponse = bool（spec friend.rs L133）。
            // 即使 affected=0 也算成功——前端意图（让对方从 pending 列表消失）已达成。
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
