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

use crate::repository::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::message::reaction::MessageReactionRemoveRequest;
use serde_json::{json, Value};

/// 处理 移除 Reaction 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 移除 Reaction 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageReactionRemoveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let user_id = request.user_id;
    let message_id = request.server_message_id;
    let requested_emoji = &request.emoji;

    // Handler 只返回 data 负载，外层 code/message 由 RPC 层封装；协议约定 data 为裸 bool
    match services
        .reaction_service
        .remove_reaction(message_id, user_id)
        .await
    {
        Ok(removed) => {
            if let Ok(Some(message)) = services
                .message_repository
                .as_ref()
                .find_by_id(message_id)
                .await
            {
                let channel_id = message.channel_id;
                let channel_type = services
                    .channel_service
                    .get_channel(&channel_id)
                    .await
                    .map(|channel| channel.channel_type.to_i16() as u8)
                    .unwrap_or(1);
                let payload = json!({
                    "message_id": message_id,
                    "channel_id": channel_id,
                    "channel_type": channel_type,
                    "uid": user_id,
                    "emoji": removed
                        .as_ref()
                        .map(|reaction| reaction.emoji.clone())
                        .unwrap_or_else(|| requested_emoji.clone()),
                    "deleted": true,
                });
                if let Err(e) = services
                    .sync_service
                    .append_server_event_commit(
                        channel_id,
                        channel_type,
                        "message_reaction",
                        payload,
                        user_id,
                    )
                    .await
                {
                    tracing::warn!("⚠️ 写入 reaction remove pts commit 失败: {}", e);
                }
            }
            tracing::debug!(
                "✅ 成功移除 Reaction: user={}, message={}, emoji={}",
                user_id,
                message_id,
                removed
                    .as_ref()
                    .map(|reaction| reaction.emoji.as_str())
                    .unwrap_or(requested_emoji)
            );
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 移除 Reaction 失败: user={}, message={}, error={}",
                user_id,
                message_id,
                e
            );
            Err(RpcError::internal(format!("移除 Reaction 失败: {}", e)))
        }
    }
}
