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
use privchat_protocol::rpc::GroupRoleSetRequest;
use serde_json::{json, Value};

/// 处理 设置成员角色 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 设置成员角色 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let request: GroupRoleSetRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let group_id = request.group_id;
    let user_id = request.user_id;
    let role_str = request.role;

    // 🔴 这个 handler 原来**一处权限校验都没有**：`operator_id` 从请求体读出来后
    // 再也没被用过，任何登录用户都能把任意人（包括自己）设成任意群的管理员。
    // 操作者只认连接上下文；请求体里的 operator_id 只能用来核对。
    let operator_id = crate::rpc::get_current_user_id(&ctx)?;
    if request.operator_id != 0 && request.operator_id != operator_id {
        return Err(RpcError::forbidden(
            "operator_id 与当前登录用户不一致".to_string(),
        ));
    }

    // 验证角色值
    let target_role = match role_str.as_str() {
        "admin" => crate::model::channel::MemberRole::Admin,
        "member" => crate::model::channel::MemberRole::Member,
        _ => {
            return Err(RpcError::validation(format!(
                "Invalid role: {}, expected 'admin' or 'member'",
                role_str
            )))
        }
    };

    // 只有群主可以任免管理员。管理员不能自我复制，也不能互相罢免。
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;
    let operator = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;
    if !matches!(operator.role, crate::model::channel::MemberRole::Owner) {
        return Err(RpcError::forbidden(
            "只有群主可以任免管理员".to_string(),
        ));
    }

    // 调用 Channel 服务设置角色
    match services
        .channel_service
        // 🔴 走落库版本：原来用的是只改内存缓存的 `set_member_role`，
        // 角色重启即回退，而鉴权在别处读的是 DB 真源——两边说的不是一件事。
        // 这个版本还会拒绝把成员改成 Owner / 改动现任 Owner（那要走转让流程）。
        .set_member_role_admin(group_id, user_id, target_role)
        .await
    {
        Ok(()) => {
            tracing::debug!(
                "✅ 成功设置成员角色: group={}, user={}, role={}",
                group_id,
                user_id,
                role_str
            );
            Ok(json!({
                "success": true,
                "group_id": group_id,
                "user_id": user_id,
                "role": role_str,
                "message": "成员角色设置成功"
            }))
        }
        Err(e) => {
            tracing::error!(
                "❌ 设置成员角色失败: group={}, user={}, error={}",
                group_id,
                user_id,
                e
            );
            Err(RpcError::internal(format!("设置成员角色失败: {}", e)))
        }
    }
}
