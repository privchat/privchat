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
use serde_json::{json, Value};

/// 处理 转让群主 请求
///
/// RPC: group/role/transfer_owner
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "current_owner_id": "alice",  // 当前群主ID
///   "new_owner_id": "bob"          // 新群主ID
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 转让群主 请求: {:?}", body);

    let group_id_str = body
        .get("group_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("group_id is required".to_string()))?;
    let group_id = group_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid group_id: {}", group_id_str)))?;

    let current_owner_id_str = body
        .get("current_owner_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("current_owner_id is required".to_string()))?;
    let current_owner_id = current_owner_id_str.parse::<u64>().map_err(|_| {
        RpcError::validation(format!(
            "Invalid current_owner_id: {}",
            current_owner_id_str
        ))
    })?;

    let new_owner_id_str = body
        .get("new_owner_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("new_owner_id is required".to_string()))?;
    let new_owner_id = new_owner_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid new_owner_id: {}", new_owner_id_str)))?;

    // 1. 获取群组
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 2. 验证当前用户是群主
    let current_member = channel
        .members
        .get(&current_owner_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        current_member.role,
        crate::model::channel::MemberRole::Owner
    ) {
        return Err(RpcError::forbidden("只有群主可以转让群主权限".to_string()));
    }

    // 3. 验证新群主是群成员
    if !channel.members.contains_key(&new_owner_id) {
        return Err(RpcError::validation("新群主不是群组成员".to_string()));
    }

    // 4. 转让群主权限
    // 将当前群主设置为普通成员
    services
        .channel_service
        .set_member_role(
            &group_id,
            &current_owner_id,
            crate::model::channel::MemberRole::Member,
        )
        .await
        .map_err(|e| RpcError::internal(format!("更新当前群主角色失败: {}", e)))?;

    // 将新成员设置为群主
    services
        .channel_service
        .set_member_role(
            &group_id,
            &new_owner_id,
            crate::model::channel::MemberRole::Owner,
        )
        .await
        .map_err(|e| RpcError::internal(format!("设置新群主失败: {}", e)))?;

    tracing::debug!(
        "✅ 转让群主成功: group_id={}, {} -> {}",
        group_id,
        current_owner_id,
        new_owner_id
    );

    // TODO: 通知所有群成员

    Ok(json!({
        "success": true,
        "group_id": group_id_str,
        "previous_owner": current_owner_id_str,
        "new_owner": new_owner_id_str,
        "message": "群主转让成功",
        "transferred_at": chrono::Utc::now().timestamp_millis()
    }))
}
