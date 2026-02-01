use serde_json::Value;
use tracing::{debug, info};

use crate::rpc::{RpcContext, RpcServiceContext, RpcResult, RpcError};
use privchat_protocol::presence::*;

/// RPC Handler: presence/status/get
/// 
/// 批量获取用户的在线状态（用于好友列表等场景）
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    _ctx: RpcContext,
) -> RpcResult<Value> {
    // 1. 解析请求参数
    let req: GetOnlineStatusRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;
    
    debug!("📥 presence/status/get: querying {} users", req.user_ids.len());
    
    // 2. 验证参数
    if req.user_ids.is_empty() {
        return Err(RpcError::validation("user_ids cannot be empty".to_string()));
    }
    
    if req.user_ids.len() > 100 {
        return Err(RpcError::validation(
            "Cannot query more than 100 users at once".to_string()
        ));
    }
    
    // 3. 批量查询
    let statuses = services.presence_manager
        .batch_get_status(req.user_ids)
        .await;
    
    // 4. 返回响应
    let response = GetOnlineStatusResponse {
        code: 0,
        message: "OK".to_string(),
        statuses,
    };
    
    info!(
        "✅ Returned online status for {} users",
        response.statuses.len()
    );
    
    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)))
}
