use crate::rpc::RpcServiceContext;
use crate::rpc::error::{RpcError, RpcResult};
use serde_json::{json, Value};
use tracing::{debug, info};

/// RPC: device/list - 查询用户的所有设备
/// 
/// 请求:
/// ```json
/// {
///   "user_id": "alice"  // 从 token 中提取或请求参数
/// }
/// ```
/// 
/// 响应:
/// ```json
/// {
///   "devices": [
///     {
///       "device_id": "xxx",
///       "device_name": "我的 iPhone",
///       "device_model": "iPhone 15 Pro",
///       "app_id": "ios",
///       "device_type": "mobile",
///       "last_active_at": "2026-01-09T12:00:00Z",
///       "created_at": "2026-01-01T10:00:00Z",
///       "ip_address": "192.168.1.100",
///       "is_current": true
///     }
///   ],
///   "total": 2
/// }
/// ```
pub async fn handle(ctx: &RpcServiceContext, params: Value) -> RpcResult<Value> {
    debug!("RPC device/list 请求: {:?}", params);
    
    // 1. 提取用户ID
    let user_id = params
        .get("user_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation(
            "缺少 user_id 参数".to_string()
        ))?;
    
    // 2. 获取当前设备ID（如果有）
    let current_device_id = params
        .get("device_id")
        .and_then(|v| v.as_str());
    
    // 3. 查询设备列表
    let devices = ctx.device_manager
        .get_user_devices_with_current(user_id, current_device_id)
        .await;
    
    let total = devices.len();
    
    info!(
        "✅ device/list 成功: user_id={}, 设备数={}",
        user_id, total
    );
    
    Ok(json!({
        "devices": devices,
        "total": total
    }))
}

