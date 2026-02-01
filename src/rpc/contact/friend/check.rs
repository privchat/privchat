use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::contact::friend::FriendCheckRequest;

/// 处理 检查好友关系 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 检查好友关系 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendCheckRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let user_id = request.user_id;
    let friend_id = request.friend_id;
    
    // 检查是否是好友
    let is_friend = services.friend_service.is_friend(user_id, friend_id).await;
    
    tracing::info!("✅ 检查好友关系: {} 和 {} 是好友: {}", user_id, friend_id, is_friend);
    Ok(json!({
        "is_friend": is_friend,
        "user_id": user_id,
        "friend_id": friend_id,
    }))
}
