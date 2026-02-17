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
    let operator_id = request.operator_id;
    let user_id = request.user_id;
    let role_str = request.role;

    // 验证角色值
    let role = match role_str.as_str() {
        "admin" => 1,
        "member" => 0,
        _ => {
            return Err(RpcError::validation(format!(
                "Invalid role: {}, expected 'admin' or 'member'",
                role_str
            )))
        }
    };

    // 验证角色值
    let target_role = match role {
        1 => crate::model::channel::MemberRole::Admin,
        0 => crate::model::channel::MemberRole::Member,
        _ => return Err(RpcError::validation("Invalid role value".to_string())),
    };

    // 调用 Channel 服务设置角色
    match services
        .channel_service
        .set_member_role(&group_id, &user_id, target_role)
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
