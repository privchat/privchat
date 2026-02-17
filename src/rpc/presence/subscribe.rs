use serde_json::Value;

use crate::rpc::{get_current_user_id, RpcContext, RpcError, RpcResult, RpcServiceContext};
use privchat_protocol::rpc::presence::*;

/// RPC Handler: presence/subscribe
///
/// 批量订阅用户的在线状态（打开私聊会话时调用）
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    // 1. 获取当前用户ID
    let user_id = get_current_user_id(&ctx)?;

    // 2. 解析请求参数
    let req: SubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;

    tracing::debug!(
        "📥 presence/subscribe: user {} subscribing to {} users",
        user_id,
        req.user_ids.len()
    );

    // 3. 验证参数
    if req.user_ids.is_empty() {
        return Err(RpcError::validation("user_ids cannot be empty".to_string()));
    }

    // 4. 批量订阅并获取状态
    let mut initial_statuses = std::collections::HashMap::new();

    for target_user_id in req.user_ids {
        // 跳过自己
        if target_user_id == 0 || user_id == target_user_id {
            continue;
        }

        // 执行订阅
        match services
            .presence_manager
            .subscribe(user_id, target_user_id)
            .await
        {
            Ok(status) => {
                initial_statuses.insert(target_user_id, status);
            }
            Err(e) => {
                tracing::debug!("⚠️ Failed to subscribe user {}: {}", target_user_id, e);
                // 继续订阅其他用户，不要因为一个失败就全部失败
            }
        }
    }

    // Handler 只返回 data 负载（仅 initial_statuses），外层 code/message 由 RPC 层封装
    let response = SubscribePresenceResponse {
        initial_statuses: initial_statuses.clone(),
    };

    tracing::debug!(
        "✅ User {} subscribed to {} users",
        user_id,
        initial_statuses.len()
    );

    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)))
}
