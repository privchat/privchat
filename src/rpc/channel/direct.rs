//! channel/direct/get_or_create RPC 处理

use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::channel::{
    GetOrCreateDirectChannelRequest,
    GetOrCreateDirectChannelResponse,
};

pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    let mut request: GetOrCreateDirectChannelRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    request.user_id = crate::rpc::get_current_user_id(&ctx)?;
    let user_id = request.user_id;
    let target_user_id = request.target_user_id;

    if user_id == target_user_id {
        return Err(RpcError::validation("不能与自己创建私聊会话".to_string()));
    }

    let (channel_id, created) = services
        .channel_service
        .get_or_create_direct_channel(
            user_id,
            target_user_id,
            request.source.as_deref(),
            request.source_id.as_deref(),
        )
        .await
        .map_err(|e| RpcError::internal(format!("获取或创建会话失败: {}", e)))?;

    let response = GetOrCreateDirectChannelResponse {
        channel_id,
        created,
    };
    Ok(json!(response))
}
