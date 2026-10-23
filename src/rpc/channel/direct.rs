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

//! channel/direct/get_or_create RPC 处理

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::channel::{
    GetOrCreateDirectChannelRequest, GetOrCreateDirectChannelResponse,
};
use serde_json::{json, Value};

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let mut request: GetOrCreateDirectChannelRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    let user_id = request.user_id;
    let target_user_id = request.target_user_id;

    if user_id == target_user_id {
        return Err(RpcError::validation("不能与自己创建私聊会话".to_string()));
    }

    if request.source.as_deref() == Some(crate::service::assistant_direct::SOURCE) {
        return Err(RpcError::validation("reserved Direct source"));
    }
    let types: Vec<i16> =
        sqlx::query_scalar("SELECT user_type FROM privchat_users WHERE user_id IN ($1,$2)")
            .bind(user_id as i64)
            .bind(target_user_id as i64)
            .fetch_all(services.channel_service.pool())
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?;
    if types.contains(&3) {
        let mut channel_id = crate::service::assistant_direct::owner_channel(
            services.channel_service.pool(),
            user_id,
            target_user_id,
        )
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
        if channel_id.is_none() {
            channel_id = crate::service::assistant_direct::owner_channel(
                services.channel_service.pool(),
                target_user_id,
                user_id,
            )
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?;
        }
        let channel_id = channel_id.ok_or_else(|| RpcError::not_found("User not found"))?;
        return Ok(json!(GetOrCreateDirectChannelResponse {
            channel_id,
            created: false
        }));
    }
    // Unprovisioned type3 targets remain unreachable to ordinary clients.
    crate::rpc::helpers::ensure_reachable_user(target_user_id, &services, || {
        RpcError::not_found(format!("User '{}' not found", target_user_id))
    })
    .await?;

    let (channel_id, created) = services
        .channel_service
        .get_or_create_direct_channel(
            user_id,
            target_user_id,
            request.source.as_deref(),
            request.source_id.as_deref(),
        )
        .await
        .map_err(|e| RpcError::internal(format!("获取或创建会话失败: {}", e)))?;

    let response = GetOrCreateDirectChannelResponse {
        channel_id,
        created,
    };
    Ok(json!(response))
}
