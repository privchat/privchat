use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::BlacklistAddRequest;
use serde_json::{json, Value};

/// 处理 添加黑名单 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 添加黑名单 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let request: BlacklistAddRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = request.user_id;
    let blocked_user_id = request.blocked_user_id;

    // 添加到黑名单
    match services
        .blacklist_service
        .add_to_blacklist(
            user_id,
            blocked_user_id,
            None, // reason
        )
        .await
    {
        Ok(_entry) => {
            tracing::debug!(
                "✅ 成功添加到黑名单: user={}, blocked={}",
                user_id,
                blocked_user_id
            );
            // 简单操作，返回 true
            Ok(json!(true))
        }
        Err(e) => {
            tracing::error!(
                "❌ 添加黑名单失败: user={}, blocked={}, error={}",
                user_id,
                blocked_user_id,
                e
            );
            Err(RpcError::internal(format!("添加黑名单失败: {}", e)))
        }
    }
}
