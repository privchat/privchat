//! entity/sync_entities RPC 处理
//!
//! 按 entity_type 委托给对应 service 的业务逻辑：friend -> FriendService，group -> ChannelService。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcContext;
use crate::rpc::{get_current_user_id, RpcServiceContext};
use privchat_protocol::rpc::sync::{
    MessageStatusSyncPayload, SyncEntitiesRequest, SyncEntitiesResponse, SyncEntityItem,
    UserSettingsSyncPayload,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const SUPPORTED_ENTITY_TYPES: &[&str] = &[
    "friend",
    "user",
    "group",
    "group_member",
    "channel",
    "message_status",
    "message_read_status",
    "user_settings",
];

fn parse_scope_channel_id(scope: Option<&str>) -> Option<u64> {
    let s = scope?;
    if let Ok(v) = s.parse::<u64>() {
        return Some(v);
    }
    s.split([':', '/', '|', ','])
        .filter_map(|t| t.trim().parse::<u64>().ok())
        .next_back()
}

fn parse_scope_user_id(scope: Option<&str>) -> Option<u64> {
    let s = scope?;
    if let Ok(v) = s.parse::<u64>() {
        return Some(v);
    }
    s.split([':', '/', '|', ','])
        .filter_map(|t| t.trim().parse::<u64>().ok())
        .next_back()
}

/// 处理 entity/sync_entities 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 entity/sync_entities 请求: {:?}", body);

    let request: SyncEntitiesRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = get_current_user_id(&ctx)?;

    let since_version = request.since_version;
    let scope = request.scope.as_deref();
    let limit = request.limit.unwrap_or(100).min(200).max(1);

    let response = match request.entity_type.as_str() {
        "friend" => services
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
            .map_err(|e| RpcError::internal(format!("好友同步失败: {}", e)))?,
        "user" => {
            let mut related_user_ids = BTreeSet::new();
            related_user_ids.insert(user_id);

            let channels = services.channel_service.get_user_channels(user_id).await.channels;
            if let Some(scoped_channel_id) = parse_scope_channel_id(scope) {
                if let Some(channel) = channels.iter().find(|ch| ch.id == scoped_channel_id) {
                    for uid in channel.get_member_ids().into_iter() {
                        related_user_ids.insert(uid);
                    }
                }
            } else if let Some(scoped_user_id) = parse_scope_user_id(scope) {
                related_user_ids.insert(scoped_user_id);
            } else {
                if let Ok(friend_ids) = services.friend_service.get_friends(user_id).await {
                    for fid in friend_ids {
                        related_user_ids.insert(fid);
                    }
                }
                for channel in channels.iter() {
                    for uid in channel.get_member_ids().into_iter() {
                        related_user_ids.insert(uid);
                    }
                }
            }

            let sorted_user_ids: Vec<u64> = related_user_ids.into_iter().collect();
            let start_idx = since_version.unwrap_or(0) as usize;
            let page_ids: Vec<u64> = sorted_user_ids
                .iter()
                .skip(start_idx)
                .take(limit as usize)
                .copied()
                .collect();
            let total_consumed = start_idx + page_ids.len();
            let has_more = total_consumed < sorted_user_ids.len();

            let mut items: Vec<SyncEntityItem> = Vec::with_capacity(page_ids.len());
            for peer_user_id in page_ids {
                if let Ok(Some(profile)) = crate::rpc::helpers::get_user_profile_with_fallback(
                    peer_user_id,
                    &services.user_repository,
                    &services.cache_manager,
                )
                .await
                {
                    items.push(SyncEntityItem {
                        entity_id: peer_user_id.to_string(),
                        version: 1,
                        deleted: false,
                        payload: Some(json!({
                            "user_id": peer_user_id,
                            "uid": peer_user_id,
                            "username": profile.username,
                            "nickname": profile.nickname,
                            "avatar": profile.avatar_url.unwrap_or_default(),
                            "user_type": profile.user_type,
                            "updated_at": 1
                        })),
                    });
                }
            }

            SyncEntitiesResponse {
                items,
                next_version: total_consumed as u64,
                has_more,
                min_version: None,
            }
        }
        "group" => services
            .channel_service
            .sync_entities_page_for_groups(user_id, since_version, scope, limit)
            .await
            .map_err(|e| RpcError::internal(format!("群组同步失败: {}", e)))?,
        "channel" => services
            .channel_service
            .sync_entities_page_for_channels(user_id, since_version, scope, limit)
            .await
            .map_err(|e| RpcError::internal(format!("会话列表同步失败: {}", e)))?,
        "group_member" => services
            .channel_service
            .sync_entities_page_for_group_members(user_id, since_version, scope, limit)
            .await
            .map_err(|e| RpcError::internal(format!("群成员同步失败: {}", e)))?,
        "message_status" | "message_read_status" => {
            let since_v = since_version.unwrap_or(0);
            let (rows, next_version, has_more) = services
                .read_receipt_service
                .sync_message_status_page(user_id, since_v, parse_scope_channel_id(scope), limit)
                .await
                .map_err(|e| RpcError::internal(format!("消息状态同步失败: {}", e)))?;
            let items: Vec<SyncEntityItem> = rows
                .into_iter()
                .map(|row| {
                    let payload = serde_json::to_value(MessageStatusSyncPayload {
                        message_id: Some(row.message_id),
                        server_message_id: Some(row.message_id),
                        id: Some(row.message_id),
                        channel_id: Some(row.channel_id),
                        channel_type: None,
                        type_field: None,
                        conversation_type: None,
                        status: None,
                        readed: Some(true),
                        is_read: Some(true),
                        read: Some(true),
                    })
                    .unwrap_or_else(|_| json!({}));
                    SyncEntityItem {
                        entity_id: format!("{}:{}", row.message_id, row.user_id),
                        version: row.read_at.timestamp_millis().max(1) as u64,
                        deleted: false,
                        payload: Some(payload),
                    }
                })
                .collect();
            SyncEntitiesResponse {
                items,
                next_version,
                has_more,
                min_version: None,
            }
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
                .map(|(setting_key, value, version)| {
                    let payload = serde_json::to_value(UserSettingsSyncPayload {
                        setting_key: Some(setting_key.clone()),
                        value: Some(value),
                    })
                    .unwrap_or_else(|_| json!({}));
                    SyncEntityItem {
                        entity_id: setting_key,
                        version,
                        deleted: false,
                        payload: Some(payload),
                    }
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
            let supported = SUPPORTED_ENTITY_TYPES.join(", ");
            return Err(RpcError::validation(format!(
                "不支持的 entity_type: {}（当前支持 {}）",
                other, supported
            )));
        }
    };

    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
