use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::BlacklistRemoveRequest;

/// 处理 移除黑名单 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 移除黑名单 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let request: BlacklistRemoveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    let user_id = request.user_id;
    let blocked_user_id = request.blocked_user_id;
    
    // 从黑名单移除
    match services.blacklist_service.remove_from_blacklist(
        user_id,
        blocked_user_id,
    ).await {
        Ok(removed) => {
            if removed {
                tracing::info!("✅ 成功从黑名单移除: user={}, blocked={}", user_id, blocked_user_id);
                // 简单操作，返回 true
                Ok(json!(true))
            } else {
                tracing::warn!("⚠️ 用户 {} 的黑名单中未找到 {}", user_id, blocked_user_id);
                // 即使不在黑名单中，也返回 true（幂等操作）
                Ok(json!(true))
            }
        }
        Err(e) => {
            tracing::error!("❌ 从黑名单移除失败: user={}, blocked={}, error={}", 
                user_id, blocked_user_id, e);
            Err(RpcError::internal(format!("从黑名单移除失败: {}", e)))
        }
    }
}
