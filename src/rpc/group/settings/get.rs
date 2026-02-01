use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;

/// 处理 获取群设置 请求
/// 
/// RPC: group/settings/get
/// 
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "user_id": "alice"  // 请求者ID（需验证是否为群成员）
/// }
/// ```
/// 
/// 响应：
/// ```json
/// {
///   "group_id": "group_123",
///   "settings": {
///     "join_need_approval": false,
///     "member_can_invite": false,
///     "all_muted": false,
///     "max_members": 500,
///     "announcement": "欢迎加入群组",
///     "description": "技术交流群",
///     "created_at": "2026-01-01T00:00:00Z",
///     "updated_at": "2026-01-10T00:00:00Z"
///   }
/// }
/// ```
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 获取群设置 请求: {:?}", body);
    
    // 解析参数
    let group_id_str = body.get("group_id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("group_id is required".to_string()))?;
    let group_id = group_id_str.parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid group_id: {}", group_id_str)))?;
    
    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    // 1. 获取群组信息
    let channel = services.channel_service.get_channel(&group_id).await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;
    
    // 2. 验证用户是否为群成员
    if !channel.members.contains_key(&user_id) {
        return Err(RpcError::forbidden("您不是群组成员".to_string()));
    }
    
    // 3. 返回群设置（从 channel.settings 转换）
    // 注意：当前 Channel.settings 是 ChannelSettings，我们需要映射到 GroupSettings
    let settings = json!({
        "join_need_approval": channel.settings.as_ref()
            .map(|s| s.require_approval)
            .unwrap_or(false),
        "member_can_invite": channel.settings.as_ref()
            .map(|s| s.allow_member_invite)
            .unwrap_or(false),
        "all_muted": channel.settings.as_ref()
            .map(|s| s.is_muted)
            .unwrap_or(false),
        "max_members": channel.settings.as_ref()
            .and_then(|s| s.max_members.map(|m| m as usize))
            .or_else(|| channel.metadata.max_members)
            .unwrap_or(500),
        "announcement": channel.metadata.announcement,
        "description": channel.metadata.description,
        "created_at": channel.created_at.to_rfc3339(),
        "updated_at": channel.updated_at.to_rfc3339()
    });
    
    tracing::info!("✅ 获取群设置成功: group_id={}", group_id);
    
    Ok(json!({
        "group_id": group_id,
        "settings": settings
    }))
}

