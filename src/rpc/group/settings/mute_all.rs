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

/// 处理 设置全员禁言 请求
///
/// RPC: group/settings/mute_all
///
/// 这是一个快捷接口，专门用于快速开启/关闭全员禁言
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "operator_id": "alice",   // 操作者ID（Owner/Admin）
///   "muted": true              // true=开启全员禁言，false=关闭
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "success": true,
///   "group_id": "group_123",
///   "all_muted": true,
///   "message": "已开启全员禁言"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 设置全员禁言 请求: {:?}", body);

    // 解析参数
    let group_id_str = body
        .get("group_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("group_id is required".to_string()))?;
    let group_id = group_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid group_id: {}", group_id_str)))?;

    let operator_id_str = body
        .get("operator_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("operator_id is required".to_string()))?;
    let operator_id = operator_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid operator_id: {}", operator_id_str)))?;

    let muted = body
        .get("muted")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| RpcError::validation("muted is required".to_string()))?;

    // 1. 获取群组
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 2. 验证操作者权限（Owner 或 Admin）
    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
    ) {
        return Err(RpcError::forbidden(
            "只有群主或管理员可以设置全员禁言".to_string(),
        ));
    }

    // 3. 设置全员禁言
    services
        .channel_service
        .set_channel_all_muted(&group_id, muted)
        .await
        .map_err(|e| RpcError::internal(format!("设置全员禁言失败: {}", e)))?;

    let action = if muted { "开启" } else { "关闭" };
    tracing::debug!(
        "✅ {}全员禁言成功: group_id={}, operator_id={}",
        action,
        group_id,
        operator_id
    );

    // TODO: 通知所有群成员

    Ok(json!({
        "success": true,
        "group_id": group_id_str,
        "all_muted": muted,
        "message": format!("已{}全员禁言", action),
        "operator_id": operator_id_str,
        "updated_at": chrono::Utc::now().timestamp_millis()
    }))
}
