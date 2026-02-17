use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::message::reaction::MessageReactionAddRequest;
use serde_json::{json, Value};

/// 处理 添加 Reaction 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 添加 Reaction 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageReactionAddRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let user_id = request.user_id;
    let message_id = request.server_message_id;
    let emoji = &request.emoji;

    // Handler 只返回 data 负载，外层 code/message 由 RPC 层封装；协议约定 data 为裸 bool
    match services
        .reaction_service
        .add_reaction(message_id, user_id, &emoji)
        .await
    {
        Ok(_reaction) => {
            tracing::debug!(
                "✅ 成功添加 Reaction: user={}, message={}, emoji={}",
                user_id,
                message_id,
                emoji
            );
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 添加 Reaction 失败: user={}, message={}, error={}",
                user_id,
                message_id,
                e
            );
            Err(RpcError::internal(format!("添加 Reaction 失败: {}", e)))
        }
    }
}
