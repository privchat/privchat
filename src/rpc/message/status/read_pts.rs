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
use privchat_protocol::rpc::{MessageStatusReadPtsRequest, MessageStatusReadPtsResponse};
use serde_json::Value;

/// 处理 按 pts 推进已读 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let req: MessageStatusReadPtsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    let server_delivered_pts = services
        .read_state_service
        .get_server_delivered_pts(user_id, req.channel_id)
        .await;

    let result = services
        .read_state_service
        .mark_read_pts_with_visible(user_id, req.channel_id, req.read_pts, req.client_visible_pts)
        .await
        .map_err(|e| RpcError::internal(format!("更新已读 pts 失败: {}", e)))?;
    serde_json::to_value(MessageStatusReadPtsResponse {
        status: "success".to_string(),
        message: "已读 pts 已更新".to_string(),
        channel_id: result.channel_id,
        last_read_pts: result.last_read_pts,
        last_read_message_id: req.last_read_message_id,
        accepted_read_pts: Some(result.last_read_pts),
        server_delivered_pts: if server_delivered_pts < u64::MAX { Some(server_delivered_pts) } else { None },
    })
    .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
