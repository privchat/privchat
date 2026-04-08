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

use serde_json::Value;

use crate::rpc::{get_current_user_id, RpcContext, RpcError, RpcResult, RpcServiceContext};
use privchat_protocol::presence::*;

/// RPC Handler: presence/status/get
///
/// 批量获取用户的在线状态（用于好友列表等场景）
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let viewer_user_id = get_current_user_id(&ctx)?;

    // 1. 解析请求参数
    let req: PresenceBatchStatusRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;

    tracing::debug!(
        "📥 presence/status/get: querying {} users",
        req.user_ids.len()
    );

    // 2. 验证参数
    if req.user_ids.is_empty() {
        return Err(RpcError::validation("user_ids cannot be empty".to_string()));
    }

    if req.user_ids.len() > 100 {
        return Err(RpcError::validation(
            "Cannot query more than 100 users at once".to_string(),
        ));
    }

    // 3. 批量查询
    let response = services
        .presence_service
        .batch_get_status(viewer_user_id, req.user_ids)
        .await;

    tracing::debug!(
        "✅ Returned online status items={} denied={}",
        response.items.len(),
        response.denied_user_ids.len()
    );

    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)))
}
