use serde_json::{json, Value};
use tracing::{debug, info};

use crate::rpc::{RpcContext, RpcServiceContext, RpcResult, RpcError, get_current_user_id};
use privchat_protocol::rpc::presence::UnsubscribePresenceRequest;

/// RPC Handler: presence/unsubscribe
///
/// 批量取消订阅。Handler 只返回 data 负载（协议为 bool）；外层 code/message 由 RPC 层封装。
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = get_current_user_id(&ctx)?;

    let req: UnsubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;

    debug!(
        "📥 presence/unsubscribe: user {} unsubscribing from {} users",
        user_id, req.user_ids.len()
    );

    for target_user_id in req.user_ids {
        if target_user_id == 0 || user_id == target_user_id {
            continue;
        }
        services.presence_manager.unsubscribe(user_id, target_user_id);
    }

    info!("✅ User {} unsubscribed from users", user_id);

    Ok(json!(true))
}
