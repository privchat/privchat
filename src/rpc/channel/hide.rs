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
use privchat_protocol::rpc::channel::ChannelHideRequest;
use serde_json::{json, Value};

/// 处理隐藏频道请求
///
/// 隐藏频道不会删除频道，只是不在用户的会话列表中显示。
/// 好友关系和群组关系仍然保留。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理隐藏频道请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: ChannelHideRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let user_id = request.user_id;
    let channel_id = request.channel_id;

    // 调用 ChannelService.hide_channel
    match services
        .channel_service
        .hide_channel(user_id, channel_id, true)
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 用户 {} 隐藏频道 {} 成功", user_id, channel_id);
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!("❌ 隐藏频道失败: {}", e);
            Err(RpcError::internal(format!("隐藏频道失败: {}", e)))
        }
    }
}
