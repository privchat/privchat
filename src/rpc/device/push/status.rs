use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::rpc::RpcContext;
use privchat_protocol::rpc::device::{DevicePushStatusRequest, DevicePushStatusResponse, DevicePushInfo};
use tracing::debug;

/// RPC Handler: device/push/status
/// 
/// 获取设备推送状态
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    debug!("RPC device/push/status 请求: {:?}", body);
    
    // 1. 解析请求
    let request: DevicePushStatusRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 2. 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    // 3. 查询设备状态
    let devices = if let Some(device_id) = &request.device_id {
        // 查询单个设备
        if let Some(device) = services.user_device_repo.as_ref()
            .get_device(user_id, device_id)
            .await
            .map_err(|e| RpcError::internal(format!("查询设备失败: {}", e)))?
        {
            vec![device]
        } else {
            return Err(RpcError::not_found("设备不存在".to_string()));
        }
    } else {
        // 查询所有设备
        services.user_device_repo.as_ref()
            .get_user_devices(user_id)
            .await
            .map_err(|e| RpcError::internal(format!("查询设备列表失败: {}", e)))?
    };
    
    // 4. 转换为响应格式
    let device_infos: Vec<DevicePushInfo> = devices.into_iter().map(|device| {
        DevicePushInfo {
            device_id: device.device_id,
            apns_armed: device.apns_armed,
            connected: device.connected,
            platform: device.platform,
            vendor: device.vendor.as_str().to_string(),
        }
    }).collect();
    
    // 5. 检查用户级别推送状态
    let user_push_enabled = services.user_device_repo.as_ref()
        .check_user_push_enabled(user_id)
        .await
        .map_err(|e| RpcError::internal(format!("检查用户推送状态失败: {}", e)))?;
    
    // 6. 返回响应
    let response = DevicePushStatusResponse {
        devices: device_infos,
        user_push_enabled,
    };
    
    Ok(json!(response))
}
