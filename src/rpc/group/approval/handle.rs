use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 审批加群请求 请求
///
/// RPC: group/approval/handle
///
/// 请求参数：
/// ```json
/// {
///   "request_id": "req_456",
///   "operator_id": "alice",         // 操作者ID（Owner/Admin）
///   "action": "approve",             // "approve" | "reject"
///   "reject_reason": "不符合要求"    // 可选，拒绝时的原因
/// }
/// ```
///
/// 响应（同意）：
/// ```json
/// {
///   "success": true,
///   "request_id": "req_456",
///   "action": "approved",
///   "group_id": "group_123",
///   "user_id": "bob",
///   "message": "已同意加群申请"
/// }
/// ```
///
/// 响应（拒绝）：
/// ```json
/// {
///   "success": true,
///   "request_id": "req_456",
///   "action": "rejected",
///   "reject_reason": "不符合要求",
///   "message": "已拒绝加群申请"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 审批加群请求 请求: {:?}", body);

    // 解析参数
    let request_id = body
        .get("request_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("request_id is required".to_string()))?;
    let operator_id = body
        .get("operator_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("operator_id is required".to_string()))?;
    let action = body
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("action is required".to_string()))?;
    let reject_reason = body
        .get("reject_reason")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 1. 验证 action 参数
    if action != "approve" && action != "reject" {
        return Err(RpcError::validation(
            "action must be 'approve' or 'reject'".to_string(),
        ));
    }

    // 2. 获取加群请求
    let request = services
        .approval_service
        .get_request(request_id)
        .await
        .map_err(|e| RpcError::internal(format!("获取请求失败: {}", e)))?
        .ok_or_else(|| RpcError::not_found("加群请求不存在".to_string()))?;

    // 3. 获取群组
    let channel = services
        .channel_service
        .get_channel(&request.group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 4. 验证操作者权限（Owner 或 Admin）
    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
    ) {
        return Err(RpcError::forbidden(
            "只有群主或管理员可以审批加群申请".to_string(),
        ));
    }

    // 5. 根据 action 执行审批
    if action == "approve" {
        // 5.1. 同意申请
        let updated_request = services
            .approval_service
            .approve_request(request_id, operator_id)
            .await
            .map_err(|e| RpcError::internal(format!("审批失败: {}", e)))?;

        // 5.2. 添加用户到群组
        services
            .channel_service
            .join_channel(
                request.group_id,
                request.user_id,
                Some(crate::model::channel::MemberRole::Member),
            )
            .await
            .map_err(|e| RpcError::internal(format!("添加用户到群组失败: {}", e)))?;

        tracing::debug!(
            "✅ 同意加群申请: request_id={}, user_id={}, group_id={}",
            request_id,
            request.user_id,
            request.group_id
        );

        // TODO: 通知申请人和群组成员

        Ok(json!({
            "success": true,
            "request_id": request_id,
            "action": "approved",
            "group_id": updated_request.group_id,
            "user_id": updated_request.user_id,
            "message": "已同意加群申请",
            "handled_at": chrono::Utc::now().to_rfc3339()
        }))
    } else {
        // 5.3. 拒绝申请
        let updated_request = services
            .approval_service
            .reject_request(request_id, operator_id, reject_reason.clone())
            .await
            .map_err(|e| RpcError::internal(format!("审批失败: {}", e)))?;

        tracing::debug!(
            "✅ 拒绝加群申请: request_id={}, user_id={}, reason={:?}",
            request_id,
            request.user_id,
            reject_reason
        );

        // TODO: 通知申请人

        Ok(json!({
            "success": true,
            "request_id": request_id,
            "action": "rejected",
            "group_id": updated_request.group_id,
            "user_id": updated_request.user_id,
            "reject_reason": reject_reason,
            "message": "已拒绝加群申请",
            "handled_at": chrono::Utc::now().to_rfc3339()
        }))
    }
}
