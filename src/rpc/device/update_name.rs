use crate::rpc::RpcServiceContext;
use crate::rpc::error::{RpcError, RpcResult};
use serde_json::{json, Value};
use tracing::{debug, info};

/// RPC: device/update_name - 更新设备名称
/// 
/// 请求:
/// ```json
/// {
///   "device_id": "device-uuid-xxx",
///   "device_name": "老婆的 iPhone"
/// }
/// ```
/// 
/// 响应:
/// ```json
/// {
///   "success": true
/// }
/// ```
pub async fn handle(ctx: &RpcServiceContext, params: Value) -> RpcResult<Value> {
    debug!("RPC device/update_name 请求: {:?}", params);
    
    // 1. 提取设备ID和新名称
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation(
            "缺少 device_id 参数".to_string()
        ))?;
    
    let device_name = params
        .get("device_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation(
            "缺少 device_name 参数".to_string()
        ))?;
    
    // 2. 验证设备名称
    if device_name.is_empty() {
        return Err(RpcError::validation(
            "device_name 不能为空".to_string()
        ));
    }
    
    if device_name.len() > 100 {
        return Err(RpcError::validation(
            "device_name 长度不能超过 100".to_string()
        ));
    }
    
    // 3. 更新设备名称
    ctx.device_manager
        .update_device_name(device_id, device_name.to_string())
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    
    info!(
        "✅ device/update_name 成功: device_id={}, new_name={}",
        device_id, device_name
    );
    
    Ok(json!({
        "success": true
    }))
}

