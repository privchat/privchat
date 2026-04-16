use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::contact::friend::FriendSetAliasRequest;
use serde_json::Value;

/// 处理 设置好友备注 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let request: FriendSetAliasRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let friend_id = request.user_id;
    let alias = request.alias;

    let success = services
        .friend_service
        .set_alias(user_id, friend_id, alias.clone())
        .await
        .map_err(|e| RpcError::internal(format!("设置备注失败: {}", e)))?;

    Ok(serde_json::to_value(success).unwrap())
}
