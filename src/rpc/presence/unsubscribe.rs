use serde_json::{json, Value};
use tracing::{debug, info};

use crate::rpc::{RpcContext, RpcServiceContext, RpcResult, RpcError, get_current_user_id};
use privchat_protocol::rpc::presence::*;

/// RPC Handler: presence/unsubscribe
/// 
/// 批量取消订阅用户的在线状态（关闭私聊会话时调用）
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    // 1. 获取当前用户ID
    let user_id = get_current_user_id(&ctx)?;
    
    // 2. 解析请求参数
    let req: UnsubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;
    
    debug!(
        "📥 presence/unsubscribe: user {} unsubscribing from {} users",
        user_id, req.user_ids.len()
    );
    
    // 3. 批量取消订阅
    for target_user_id in req.user_ids {
        if target_user_id == 0 || user_id == target_user_id {
            continue;
        }
        services.presence_manager.unsubscribe(user_id, target_user_id);
    }
    
    // 4. 返回响应（简单操作，返回 true）
    info!(
        "✅ User {} unsubscribed from users",
        user_id
    );
    
    Ok(json!(true))
}
