use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::message::reaction::MessageReactionAddRequest;

/// 处理 添加 Reaction 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 添加 Reaction 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageReactionAddRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let user_id = request.user_id;
    let message_id = request.server_message_id;
    let emoji = &request.emoji;
    
    // 调用 Reaction 服务
    match services.reaction_service.add_reaction(
        message_id,
        user_id,
        &emoji,
    ).await {
        Ok(_reaction) => {
            tracing::info!("✅ 成功添加 Reaction: user={}, message={}, emoji={}", 
                user_id, message_id, emoji);
            Ok(json!({
                "success": true,
                "message": "Reaction 添加成功"
            }))
        }
        Err(e) => {
            tracing::error!("❌ 添加 Reaction 失败: user={}, message={}, error={}", 
                user_id, message_id, e);
            Err(RpcError::internal(format!("添加 Reaction 失败: {}", e)))
        }
    }
}
