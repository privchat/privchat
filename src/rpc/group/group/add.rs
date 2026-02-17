use crate::rpc::error::{RpcError, RpcResult};
use serde_json::{json, Value};

/// 处理群组添加用户请求
pub async fn handle(body: Value) -> RpcResult<Value> {
    let group_id = body
        .get("group_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("group_id is required".to_string()))?;

    // 注意：这里的 user_id 是要添加到群组的用户，不是操作者
    // 保持从请求体获取
    let user_id = body
        .get("user_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("user_id is required".to_string()))?;

    let inviter_id = body
        .get("inviter_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("inviter_id is required".to_string()))?;

    // TODO: 实际的群组添加用户逻辑
    // 1. 验证群组是否存在
    // 2. 验证邀请者是否有权限
    // 3. 检查用户是否已经在群组中
    // 4. 添加用户到群组
    // 5. 发送通知

    tracing::debug!(
        "👥 添加用户到群组: group_id={}, user_id={}, inviter_id={}",
        group_id,
        user_id,
        inviter_id
    );

    // 模拟添加成功
    Ok(json!({
        "group_id": group_id,
        "user_id": user_id,
        "inviter_id": inviter_id,
        "joined_at": chrono::Utc::now().to_rfc3339(),
        "role": "member"
    }))
}
