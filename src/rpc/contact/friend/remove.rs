use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::contact::friend::FriendRemoveRequest;

/// 处理 删除好友 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 删除好友 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: FriendRemoveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let user_id = request.user_id;
    let friend_id = request.friend_id;
    
    // 删除好友关系
    match services.friend_service.remove_friend(user_id, friend_id).await {
        Ok(_) => {
            tracing::info!("✅ 好友删除成功: {} <-> {}", user_id, friend_id);
            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!("❌ 删除好友失败: {}", e);
            Err(RpcError::internal(format!("删除好友失败: {}", e)))
        }
    }
}
