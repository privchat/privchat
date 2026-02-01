use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;

/// 处理 消息计数 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 消息计数 请求: {:?}", body);
    
    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let channel_id_str = body.get("channel_id").and_then(|v| v.as_str());
    let channel_id = channel_id_str.and_then(|s| s.parse::<u64>().ok());
    
    // 如果指定了频道，返回该频道的未读计数
    if let Some(ch_id) = channel_id {
        match services.channel_service.get_user_channel(&user_id, &ch_id).await {
            Ok(conv) => {
                Ok(json!({
                    "unread_count": conv.unread_count,
                    "channel_id": channel_id_str.unwrap(),
                }))
            }
            Err(_) => {
                // 频道不存在或用户不在频道中，返回0
                Ok(json!({
                    "unread_count": 0,
                    "channel_id": channel_id_str.unwrap(),
                }))
            }
        }
    } else {
        // 返回用户的总未读计数
        match services.channel_service.get_user_unread_count(&user_id).await {
            Ok(count) => {
                Ok(json!({
                    "unread_count": count,
                }))
            }
            Err(e) => {
                tracing::error!("❌ 获取未读计数失败: {}", e);
                Err(RpcError::internal(format!("获取未读计数失败: {}", e)))
            }
        }
    }
}
