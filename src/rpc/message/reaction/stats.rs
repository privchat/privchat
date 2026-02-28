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
use privchat_protocol::rpc::message::reaction::MessageReactionStatsRequest;
use serde_json::{json, Value};

/// 处理 获取 Reaction 统计 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 获取 Reaction 统计 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageReactionStatsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let message_id = request.server_message_id;

    // 调用 Reaction 服务
    match services
        .reaction_service
        .get_message_reactions(message_id)
        .await
    {
        Ok(stats) => {
            tracing::debug!("✅ 成功获取 Reaction 统计: message={}", message_id);
            Ok(json!({
                "success": true,
                "stats": stats
            }))
        }
        Err(e) => {
            tracing::error!(
                "❌ 获取 Reaction 统计失败: message={}, error={}",
                message_id,
                e
            );
            Err(RpcError::internal(format!("获取 Reaction 统计失败: {}", e)))
        }
    }
}
