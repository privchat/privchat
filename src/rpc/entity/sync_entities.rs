//! entity/sync_entities RPC 处理
//!
//! 按 entity_type 委托给对应 service 的业务逻辑：friend -> FriendService，group -> ChannelService。

use serde_json::{Value, json};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{RpcServiceContext, get_current_user_id};
use crate::rpc::RpcContext;
use privchat_protocol::rpc::sync::{SyncEntitiesRequest, SyncEntitiesResponse, SyncEntityItem};

/// 处理 entity/sync_entities 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 entity/sync_entities 请求: {:?}", body);

    let request: SyncEntitiesRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = get_current_user_id(&ctx)?;

    let since_version = request.since_version;
    let scope = request.scope.as_deref();
    let limit = request.limit.unwrap_or(100).min(200).max(1);

    let response = match request.entity_type.as_str() {
        "friend" => {
            services
                .friend_service
                .sync_entities_page(
                    user_id,
                    since_version,
                    scope,
                    limit,
                    &services.user_repository,
                    &services.cache_manager,
                )
                .await
                .map_err(|e| RpcError::internal(format!("好友同步失败: {}", e)))?
        }
        "group" => {
            services
                .channel_service
                .sync_entities_page_for_groups(user_id, since_version, scope, limit)
                .await
                .map_err(|e| RpcError::internal(format!("群组同步失败: {}", e)))?
        }
        "channel" => {
            services
                .channel_service
                .sync_entities_page_for_channels(user_id, since_version, scope, limit)
                .await
                .map_err(|e| RpcError::internal(format!("会话列表同步失败: {}", e)))?
        }
        "user_settings" => {
            let since_v = since_version.unwrap_or(0);
            let (list, next_version, has_more) = services
                .user_settings_repo
                .get_since(user_id, since_v, limit)
                .await
                .map_err(|e| RpcError::internal(format!("用户设置同步失败: {}", e)))?;
            let items: Vec<SyncEntityItem> = list
                .into_iter()
                .map(|(setting_key, value, version)| SyncEntityItem {
                    entity_id: setting_key,
                    version,
                    deleted: false,
                    payload: Some(json!({ "value": value })),
                })
                .collect();
            SyncEntitiesResponse {
                items,
                next_version,
                has_more,
                min_version: None,
            }
        }
        other => {
            return Err(RpcError::validation(format!(
                "不支持的 entity_type: {}（当前支持 friend, group, channel, user_settings）",
                other
            )));
        }
    };

    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
