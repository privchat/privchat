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

/// 处理 获取加群审批列表 请求
///
/// RPC: group/approval/list
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "operator_id": "alice"  // 操作者ID（需验证是Owner/Admin）
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "group_id": "group_123",
///   "requests": [
///     {
///       "request_id": "req_456",
///       "user_id": "bob",
///       "method": {
///         "QRCode": { "qr_code_id": "qr_789" }
///       },
///       "message": "我想加入",
///       "created_at": "2026-01-10T12:00:00Z",
///       "expires_at": "2026-01-11T12:00:00Z"
///     }
///   ],
///   "total": 1
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 获取加群审批列表 请求: {:?}", body);

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
            "只有群主或管理员可以查看审批列表".to_string(),
        ));
    }

    // 3. 获取待审批请求
    let requests = services
        .approval_service
        .get_pending_requests_by_group(group_id)
        .await
        .map_err(|e| RpcError::internal(format!("获取审批列表失败: {}", e)))?;

    // 4. 转换为 JSON 格式
    let requests_json: Vec<Value> = requests
        .iter()
        .map(|req| {
            json!({
                "request_id": req.request_id,
                "user_id": req.user_id,
                "method": match &req.method {
                    crate::service::JoinMethod::MemberInvite { inviter_id } => {
                        json!({"MemberInvite": {"inviter_id": inviter_id}})
                    },
                    crate::service::JoinMethod::QRCode { qr_code_id } => {
                        json!({"QRCode": {"qr_code_id": qr_code_id}})
                    }
                },
                "message": req.message,
                "created_at": req.created_at.to_rfc3339(),
                "expires_at": req.expires_at.map(|dt| dt.to_rfc3339())
            })
        })
        .collect();

    tracing::debug!(
        "✅ 获取加群审批列表成功: group_id={}, count={}",
        group_id,
        requests.len()
    );

    Ok(json!({
        "group_id": group_id_str,
        "requests": requests_json,
        "total": requests.len()
    }))
}
