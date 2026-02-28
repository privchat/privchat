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
use privchat_protocol::rpc::group::member::GroupMemberUnmuteRequest;
use serde_json::{json, Value};

/// 处理 取消禁言 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 取消禁言 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupMemberUnmuteRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 operator_id
    request.operator_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let operator_id = request.operator_id;
    let user_id = request.user_id;

    // 调用 Channel 服务取消禁言
    match services
        .channel_service
        .set_member_muted(&group_id, &user_id, false, None)
        .await
    {
        Ok(()) => {
            tracing::debug!("✅ 成功取消禁言: group={}, user={}", group_id, user_id);
            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 取消禁言失败: group={}, user={}, error={}",
                group_id,
                user_id,
                e
            );
            Err(RpcError::internal(format!("取消禁言失败: {}", e)))
        }
    }
}
