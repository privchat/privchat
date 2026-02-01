use crate::rpc::RpcServiceContext;
use crate::rpc::error::{RpcError, RpcResult};
use serde_json::{json, Value};
use tracing::{debug, info};

/// RPC: device/revoke_all - 退出所有设备（撤销用户的所有 token）
/// 
/// 请求:
/// ```json
/// {
///   "user_id": "alice"
/// }
/// ```
/// 
/// 响应:
/// ```json
/// {
///   "success": true,
///   "revoked_count": 3
/// }
/// ```
pub async fn handle(ctx: &RpcServiceContext, params: Value) -> RpcResult<Value> {
    debug!("RPC device/revoke_all 请求: {:?}", params);
    
    // 1. 提取用户ID
    let user_id = params
        .get("user_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation(
            "缺少 user_id 参数".to_string()
        ))?;
    
    // 2. 撤销所有设备
    let revoked_count = ctx.token_revocation_service
        .revoke_all_devices(user_id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    
    info!(
        "✅ device/revoke_all 成功: user_id={}, revoked_count={}",
        user_id, revoked_count
    );
    
    Ok(json!({
        "success": true,
        "revoked_count": revoked_count
    }))
}

