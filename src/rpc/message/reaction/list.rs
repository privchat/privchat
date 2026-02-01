use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::message::reaction::MessageReactionListRequest;

/// 处理 获取 Reaction 列表 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 获取 Reaction 列表 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageReactionListRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let message_id = request.server_message_id;
    
    // 调用 Reaction 服务
    match services.reaction_service.get_message_reactions(message_id).await {
        Ok(stats) => {
            tracing::info!("✅ 成功获取 Reaction 列表: message={}, total={}", 
                message_id, stats.total_count);
            Ok(json!({
                "success": true,
                "reactions": stats.reactions,
                "total_count": stats.total_count
            }))
        }
        Err(e) => {
            tracing::error!("❌ 获取 Reaction 列表失败: message={}, error={}", message_id, e);
            Err(RpcError::internal(format!("获取 Reaction 列表失败: {}", e)))
        }
    }
}
