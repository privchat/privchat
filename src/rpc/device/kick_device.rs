use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;

/// 踢出指定设备请求
#[derive(Debug, Deserialize)]
pub struct KickDeviceRequest {
    /// 要踢出的设备ID
    pub device_id: String,
    
    /// 踢出原因（可选）
    #[serde(default)]
    pub reason: Option<String>,
}

/// 踢出指定设备响应
#[derive(Debug, Serialize)]
pub struct KickDeviceResponse {
    pub success: bool,
    pub message: String,
}

/// 处理"踢出指定设备"请求
/// 
/// 管理员或用户可以踢出指定的设备。
/// 
/// 请求示例：
/// ```json
/// {
///   "device_id": "uuid",
///   "reason": "suspicious_activity"
/// }
/// ```
/// 
/// 响应示例：
/// ```json
/// {
///   "success": true,
///   "message": "设备已踢出"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::info!("🔧 处理踢出指定设备请求: {:?}", body);
    
    // 1. 解析请求
    let request: KickDeviceRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 2. 从 RpcContext 获取当前用户和设备ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let current_device_id = ctx.device_id
        .as_ref()
        .ok_or_else(|| RpcError::validation("缺少设备ID".to_string()))?;
    let target_device_id = &request.device_id;
    
    // 3. 验证不能踢出自己
    if target_device_id == current_device_id {
        return Err(RpcError::validation(
            "不能踢出当前设备".to_string()
        ));
    }
    
    // 4. 踢出指定设备（使用数据库版本）
    let reason = request.reason.as_deref().unwrap_or("kicked_by_user");
    
    services
        .device_manager_db
        .kick_device(
            user_id,
            target_device_id,
            Some(current_device_id),
            reason,
        )
        .await
        .map_err(|e| RpcError::internal(format!("踢出设备失败: {}", e)))?;
    
    // 5. 断开设备连接（✨ 新增）
    if let Err(e) = services
        .connection_manager
        .disconnect_device(user_id, target_device_id)
        .await
    {
        tracing::warn!(
            "⚠️ 断开设备连接失败（设备可能未在线）: user={}, device={}, error={}",
            user_id,
            target_device_id,
            e
        );
    }
    
    tracing::info!(
        "✅ 设备已踢出: user={}, device={}, reason={}",
        user_id,
        target_device_id,
        reason
    );
    
    // 5. 返回结果
    Ok(json!({
        "success": true,
        "message": "设备已踢出",
    }))
}
