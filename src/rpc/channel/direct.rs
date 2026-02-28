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
