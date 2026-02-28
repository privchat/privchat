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
use privchat_protocol::rpc::GroupMemberRemoveRequest;
use serde_json::{json, Value};

/// 处理 删除群成员 请求（管理员/群主移除他人）
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 删除群成员 请求: {:?}", body);

    // ✅ 使用 protocol 定义自动反序列化
    let request: GroupMemberRemoveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    // ✅ 从 ctx 获取 operator_id（安全性）
    // operator_id 是执行移除操作的人，必须从 ctx 获取
    let operator_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let user_id = request.user_id; // 被移除的目标用户（从请求体）

    // 验证操作者权限（必须是群主或管理员）
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("Group not found: {}", e)))?;

    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("Operator is not a member".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
    ) {
        return Err(RpcError::forbidden(
            "Only owner or admin can remove members".to_string(),
        ));
    }

    // 验证被移除者不是群主
    if let Some(target_member) = channel.members.get(&user_id) {
        if matches!(target_member.role, crate::model::channel::MemberRole::Owner) {
            return Err(RpcError::forbidden("Cannot remove group owner".to_string()));
        }
    }

    // 移除成员
    match services
        .channel_service
        .leave_channel(group_id, user_id)
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 成功移除成员 {} 从群组 {}", user_id, group_id);
            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!("❌ 移除群成员失败: {}", e);
            Err(RpcError::internal(format!("移除群成员失败: {}", e)))
        }
    }
}
