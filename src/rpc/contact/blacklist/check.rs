use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::BlacklistCheckRequest;

/// 处理 检查黑名单 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 检查黑名单 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let request: BlacklistCheckRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    let user_id = request.user_id;
    let target_user_id = request.target_user_id;
    
    // 检查黑名单
    match services.blacklist_service.is_blocked(
        user_id,
        target_user_id,
    ).await {
        Ok(is_blocked) => {
            tracing::info!("✅ 黑名单检查完成: user={}, target={}, blocked={}", 
                user_id, target_user_id, is_blocked);
            Ok(json!({
                "success": true,
                "blocked": is_blocked
            }))
        }
        Err(e) => {
            tracing::error!("❌ 检查黑名单失败: user={}, target={}, error={}", 
                user_id, target_user_id, e);
            Err(RpcError::internal(format!("检查黑名单失败: {}", e)))
        }
    }
}
