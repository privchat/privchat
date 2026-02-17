use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::BlacklistListRequest;
use serde_json::{json, Value};

/// 处理 获取黑名单列表 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 获取黑名单列表 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let request: BlacklistListRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = request.user_id;

    // 获取黑名单列表
    match services.blacklist_service.get_blacklist(user_id).await {
        Ok(blocked_users) => {
            tracing::debug!(
                "✅ 成功获取黑名单列表: user={}, count={}",
                user_id,
                blocked_users.len()
            );
            Ok(json!({
                "success": true,
                "users": blocked_users
            }))
        }
        Err(e) => {
            tracing::error!("❌ 获取黑名单列表失败: user={}, error={}", user_id, e);
            Err(RpcError::internal(format!("获取黑名单列表失败: {}", e)))
        }
    }
}
