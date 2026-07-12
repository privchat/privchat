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

/// 读取 numeric id（u64）。**标准 = JSON number**（`as_u64`，与 typed 协议结构体 / settings-get /
/// approval-handle 一致）；为向后兼容旧请求，同时容忍 string 形态（`parse`）。
/// 硬规则：user_id / operator_id / group_id / channel_id 等 uid 类 id 全局 u64；
/// 只有真正的 opaque id（如 approval `request_id` 是 UUID）才用 String。
fn read_u64_id(body: &Value, key: &str) -> RpcResult<u64> {
    body.get(key)
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse::<u64>().ok()))
        .ok_or_else(|| RpcError::validation(format!("{} is required (u64)", key)))
}

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

    // 解析参数（标准 u64 number；string 向后兼容）。此前按 as_str 读，与协议/其他 handler 不一致，
    // 导致 typed FFI / TS SDK 送的 number id 命中不到 → list 运行期失败（已修）。
    let group_id = read_u64_id(&body, "group_id")?;
    let operator_id = read_u64_id(&body, "operator_id")?;

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
                "created_at": req.created_at.timestamp_millis(),
                "expires_at": req.expires_at.map(|dt| dt.timestamp_millis())
            })
        })
        .collect();

    tracing::debug!(
        "✅ 获取加群审批列表成功: group_id={}, count={}",
        group_id,
        requests.len()
    );

    Ok(json!({
        "group_id": group_id.to_string(),
        "requests": requests_json,
        "total": requests.len()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn read_u64_id_accepts_number_standard() {
        let body = json!({ "group_id": 1234567890123u64, "operator_id": 42 });
        assert_eq!(read_u64_id(&body, "group_id").unwrap(), 1234567890123u64);
        assert_eq!(read_u64_id(&body, "operator_id").unwrap(), 42);
    }

    #[test]
    fn read_u64_id_tolerates_string_for_backward_compat() {
        let body = json!({ "group_id": "1234567890123", "operator_id": "42" });
        assert_eq!(read_u64_id(&body, "group_id").unwrap(), 1234567890123u64);
        assert_eq!(read_u64_id(&body, "operator_id").unwrap(), 42);
    }

    #[test]
    fn read_u64_id_missing_or_invalid_is_error() {
        let body = json!({ "group_id": "not-a-number" });
        assert!(read_u64_id(&body, "group_id").is_err());
        assert!(read_u64_id(&body, "operator_id").is_err());
    }
}
