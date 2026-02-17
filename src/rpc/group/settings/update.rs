use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 更新群设置 请求
///
/// RPC: group/settings/update
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "operator_id": "alice",  // 操作者ID（需验证权限，仅群主）
///   "settings": {
///     "join_need_approval": true,      // 可选
///     "member_can_invite": false,      // 可选
///     "all_muted": false,              // 可选
///     "max_members": 1000,             // 可选
///     "announcement": "新公告",         // 可选
///     "description": "新描述"           // 可选
///   }
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "success": true,
///   "group_id": "group_123",
///   "message": "群设置更新成功",
///   "updated_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 更新群设置 请求: {:?}", body);

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

    let settings = body
        .get("settings")
        .ok_or_else(|| RpcError::validation("settings is required".to_string()))?;

    // 1. 获取群组信息
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 2. 验证操作者权限（仅群主可以修改群设置）
    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner
    ) {
        return Err(RpcError::forbidden("只有群主可以修改群设置".to_string()));
    }

    // 3. 提取要更新的设置
    let join_need_approval = settings.get("join_need_approval").and_then(|v| v.as_bool());
    let member_can_invite = settings.get("member_can_invite").and_then(|v| v.as_bool());
    let all_muted = settings.get("all_muted").and_then(|v| v.as_bool());
    let max_members = settings
        .get("max_members")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let announcement = settings
        .get("announcement")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let description = settings
        .get("description")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // 4. 更新群设置
    let mut update_count = 0;

    // 4.1. 更新描述
    if let Some(desc) = description {
        services
            .channel_service
            .update_channel_metadata(
                &group_id,
                None, // name不变
                Some(desc),
                None, // avatar_url不变
            )
            .await
            .map_err(|e| RpcError::internal(format!("更新群描述失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群描述成功");
    }

    // 4.2. 更新群公告
    if let Some(ann) = announcement {
        services
            .channel_service
            .set_channel_announcement(&group_id, Some(ann))
            .await
            .map_err(|e| RpcError::internal(format!("更新群公告失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群公告成功");
    }

    // 4.3. 更新全员禁言
    if let Some(muted) = all_muted {
        services
            .channel_service
            .set_channel_all_muted(&group_id, muted)
            .await
            .map_err(|e| RpcError::internal(format!("更新全员禁言失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新全员禁言成功: {}", muted);
    }

    // 4.4. 更新加群审批
    if let Some(approval) = join_need_approval {
        services
            .channel_service
            .set_channel_join_approval(&group_id, approval)
            .await
            .map_err(|e| RpcError::internal(format!("更新加群审批失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新加群审批成功: {}", approval);
    }

    // 4.5. 更新成员邀请权限
    if let Some(invite) = member_can_invite {
        services
            .channel_service
            .set_channel_member_invite(&group_id, invite)
            .await
            .map_err(|e| RpcError::internal(format!("更新成员邀请权限失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新成员邀请权限成功: {}", invite);
    }

    // 4.6. 更新成员上限
    if let Some(max) = max_members {
        services
            .channel_service
            .set_channel_max_members(&group_id, max)
            .await
            .map_err(|e| RpcError::internal(format!("更新成员上限失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新成员上限成功: {}", max);
    }

    tracing::debug!(
        "✅ 群设置更新成功: group_id={}, 更新项数={}",
        group_id,
        update_count
    );

    Ok(json!({
        "success": true,
        "group_id": group_id_str,
        "message": format!("群设置更新成功，共更新 {} 项", update_count),
        "updated_count": update_count,
        "updated_at": chrono::Utc::now().to_rfc3339()
    }))
}
