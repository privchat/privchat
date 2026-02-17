use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::GroupMemberLeaveRequest;
use serde_json::{json, Value};

/// 处理 退群 请求（用户主动离开）
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 退群 请求: {:?}", body);

    // ✅ 使用 protocol 定义自动反序列化
    let mut request: GroupMemberLeaveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    // ✅ 从 ctx 覆盖 user_id（安全性）
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let user_id = request.user_id;

    // 验证用户是群成员
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("Group not found: {}", e)))?;

    if !channel.members.contains_key(&user_id) {
        return Err(RpcError::not_found(
            "User is not a member of this group".to_string(),
        ));
    }

    // 验证用户不是群主（群主不能退群，需要转让或解散）
    if let Some(member) = channel.members.get(&user_id) {
        if matches!(member.role, crate::model::channel::MemberRole::Owner) {
            return Err(RpcError::forbidden(
                "Group owner cannot leave group. Please transfer ownership or disband the group"
                    .to_string(),
            ));
        }
    }

    // 离开群组
    match services
        .channel_service
        .leave_channel(group_id, user_id)
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 用户 {} 成功退出群组 {}", user_id, group_id);
            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!("❌ 退群失败: {}", e);
            Err(RpcError::internal(format!("退群失败: {}", e)))
        }
    }
}
