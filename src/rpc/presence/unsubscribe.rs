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

use serde_json::{json, Value};

use crate::rpc::{get_current_user_id, RpcContext, RpcError, RpcResult, RpcServiceContext};
use privchat_protocol::rpc::presence::UnsubscribePresenceRequest;

/// RPC Handler: presence/unsubscribe
///
/// 批量取消订阅。Handler 只返回 data 负载（协议为 bool）；外层 code/message 由 RPC 层封装。
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = get_current_user_id(&ctx)?;

    let req: UnsubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;

    tracing::debug!(
        "📥 presence/unsubscribe: user {} unsubscribing from {} users",
        user_id,
        req.user_ids.len()
    );

    for target_user_id in req.user_ids {
        if target_user_id == 0 || user_id == target_user_id {
            continue;
        }
        services
            .presence_manager
            .unsubscribe(user_id, target_user_id);
    }

    tracing::debug!("✅ User {} unsubscribed from users", user_id);

    Ok(json!(true))
}
