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

use privchat_protocol::rpc::message::revoke::MessageRevokeRequest;
use serde_json::{json, Value};

use crate::error::ServerError;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;

/// 处理消息撤回请求
///
/// RPC 入口：仅做参数校验 + 调用 `MessageService::revoke_message_with_auth`。
/// 权限/时限校验、DB 标记、事件发布、缓存同步、PTS commit、在线推送、离线队列清理
/// 全部在 `MessageService` 内部完成（见 `ADMIN_API_SPEC §1.4` 收敛规则）。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理消息撤回请求: {:?}", body);

    let mut request: MessageRevokeRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    tracing::debug!(
        "🔧 处理消息撤回请求: server_message_id={}, channel_id={}, user_id={}",
        request.server_message_id,
        request.channel_id,
        request.user_id
    );

    services
        .message_service
        .revoke_message_with_auth(
            request.server_message_id,
            request.channel_id,
            request.user_id,
        )
        .await
        .map_err(map_server_error)?;

    Ok(json!(true))
}

fn map_server_error(err: ServerError) -> RpcError {
    match err {
        ServerError::NotFound(msg) | ServerError::MessageNotFound(msg) => RpcError::not_found(msg),
        ServerError::Forbidden(msg) => RpcError::forbidden(msg),
        ServerError::Validation(msg) | ServerError::InvalidRequest(msg) => {
            RpcError::validation(msg)
        }
        other => RpcError::internal(other.to_string()),
    }
}
