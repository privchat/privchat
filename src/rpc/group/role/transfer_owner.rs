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

    // 🔴 操作者**只**取连接上下文。
    //
    // 原实现整段不看 `ctx`，直接信请求体里的 `current_owner_id`，再拿它去查成员表
    // 判「你是不是群主」——于是任何群成员填上真群主的 ID 就能通过判定，把群转给自己。
    // 请求体里仍允许带 `current_owner_id`（老客户端会发），但它只能用来核对，不能用来授权。
    let operator_id = crate::rpc::get_current_user_id(&ctx)?;
    if let Some(claimed) = body.get("current_owner_id").and_then(|v| v.as_str()) {
        if claimed.parse::<u64>().ok() != Some(operator_id) {
            return Err(RpcError::forbidden(
                "current_owner_id 与当前登录用户不一致".to_string(),
            ));
        }
    }

    let new_owner_id_str = body
        .get("new_owner_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("new_owner_id is required".to_string()))?;
    let new_owner_id = new_owner_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid new_owner_id: {}", new_owner_id_str)))?;

    // 群主判定与角色变更在**同一个事务**里完成：
    // 事务外先读一遍再写，中间群主可能已经换人，两次写还可能只成功一半
    // （原实现就是两次独立的 set_member_role，失败在中间会留下一个没有群主的群）。
    services
        .channel_service
        .transfer_group_owner(group_id, operator_id, new_owner_id)
        .await
        .map_err(RpcError::from)?;

    tracing::debug!(
        "✅ 转让群主成功: group_id={}, {} -> {}",
        group_id,
        operator_id,
        new_owner_id
    );

    // TODO: 通知所有群成员

    Ok(json!({
        "success": true,
        "group_id": group_id_str,
        "previous_owner": operator_id.to_string(),
        "new_owner": new_owner_id_str,
        "message": "群主转让成功",
        "transferred_at": chrono::Utc::now().timestamp_millis()
    }))
}
