// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! entity/sync_entities RPC 处理
//!
//! 按 entity_type 委托给对应 service 的业务逻辑：friend -> FriendService，group -> ChannelService。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcContext;
use crate::rpc::RpcServiceContext;
use privchat_protocol::ErrorCode;
use privchat_protocol::rpc::sync::{
    ChannelReadCursorSyncPayload, SyncEntitiesRequest, SyncEntitiesResponse, SyncEntityItem,
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

const SUPPORTED_ENTITY_TYPES: &[&str] = &[
    "friend",
    "user",
    "group",
    "group_member",
    "channel",
    "channel_read_cursor",
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

fn sync_authenticated_user_id(ctx: &RpcContext) -> RpcResult<u64> {
    let user_id_str = ctx.user_id.as_ref().ok_or_else(|| {
        RpcError::from_code(
            privchat_protocol::ErrorCode::SyncFullRebuildRequired,
            "session user missing for entity/sync_entities".to_string(),
        )
    })?;

    user_id_str.parse::<u64>().map_err(|_| {
        RpcError::from_code(
            privchat_protocol::ErrorCode::SyncFullRebuildRequired,
            "invalid session user_id for entity/sync_entities".to_string(),
        )
    })
}

fn entity_resync_required<S: Into<String>>(msg: S) -> RpcError {
    RpcError::from_code(ErrorCode::SyncEntityResyncRequired, msg.into())
}

fn validate_group_member_scope(scope: Option<&str>) -> RpcResult<()> {
    if parse_scope_channel_id(scope).is_none() {
        return Err(entity_resync_required(
            "group_member sync requires scope=group_id",
        ));
    }
    Ok(())
}

/// 处理 entity/sync_entities 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 entity/sync_entities 请求: {:?}", body);

    let request: SyncEntitiesRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = sync_authenticated_user_id(&ctx)?;

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

            let related_user_ids: Vec<u64> = related_user_ids.into_iter().collect();
            let since_v = since_version.unwrap_or(0);
            let users = services
                .user_repository
                .find_related_since(&related_user_ids, since_v, limit)
                .await
                .map_err(|e| RpcError::internal(format!("用户资料同步失败: {}", e)))?;
            let has_more = users.len() >= limit as usize;
            let mut next_version = since_v;

            let mut items: Vec<SyncEntityItem> = Vec::with_capacity(users.len());
            for entry in users {
                let user = entry.user;
                let updated_at = user.updated_at.timestamp_millis();
                next_version = next_version.max(entry.sync_version);
                items.push(SyncEntityItem {
                    entity_id: user.id.to_string(),
                    version: entry.sync_version,
                    deleted: false,
                    payload: Some(json!({
                        "user_id": user.id,
                        "uid": user.id,
                        "username": user.username,
                        "nickname": user.display_name.clone().unwrap_or_else(|| user.username.clone()),
                        "avatar": user.avatar_url.clone().unwrap_or_default(),
                        "user_type": user.user_type,
                        "status": user.status,
                        "updated_at": updated_at
                    })),
                });
            }

            SyncEntitiesResponse {
                items,
                next_version,
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
        "group_member" => {
            validate_group_member_scope(scope)?;
            services
                .channel_service
                .sync_entities_page_for_group_members(user_id, since_version, scope, limit)
                .await
                .map_err(|e| match e {
                    crate::error::ServerError::Validation(msg)
                        if msg.contains("group_member sync requires scope") =>
                    {
                        entity_resync_required(msg)
                    }
                    other => RpcError::internal(format!("群成员同步失败: {}", other)),
                })?
        }
        "channel_read_cursor" => {
            let since_v = since_version.unwrap_or(0);
            let (rows, next_version, has_more) = services
                .read_state_service
                .sync_channel_read_cursor_page(user_id, since_v, parse_scope_channel_id(scope), limit)
                .await
                .map_err(|e| RpcError::internal(format!("已读游标同步失败: {}", e)))?;
            let items: Vec<SyncEntityItem> = rows
                .into_iter()
                .map(|row| {
                    let payload = serde_json::to_value(ChannelReadCursorSyncPayload {
                        channel_id: Some(row.channel_id),
                        channel_type: Some(row.channel_type),
                        type_field: Some(row.channel_type),
                        reader_id: Some(row.reader_id),
                        last_read_pts: Some(row.last_read_pts),
                        updated_at: Some(row.version as i64),
                    })
                    .unwrap_or_else(|_| json!({}));
                    SyncEntityItem {
                        entity_id: format!("{}:{}", row.channel_id, row.reader_id),
                        version: row.version,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::RpcContext;

    #[test]
    fn supported_entity_types_match_current_entity_sync_surface() {
        assert_eq!(
            SUPPORTED_ENTITY_TYPES,
            &[
                "friend",
                "user",
                "group",
                "group_member",
                "channel",
                "channel_read_cursor",
            ]
        );
        assert!(!SUPPORTED_ENTITY_TYPES.contains(&"message"));
    }

    #[test]
    fn scope_parsers_extract_current_supported_formats() {
        assert_eq!(parse_scope_channel_id(Some("30001")), Some(30001));
        assert_eq!(parse_scope_channel_id(Some("group:30001")), Some(30001));
        assert_eq!(
            parse_scope_channel_id(Some("channel:2|30001")),
            Some(30001)
        );

        assert_eq!(parse_scope_user_id(Some("20001")), Some(20001));
        assert_eq!(parse_scope_user_id(Some("user:20001")), Some(20001));
        assert_eq!(parse_scope_user_id(Some("friend/20001")), Some(20001));
    }

    #[test]
    fn missing_sync_entities_user_requires_full_rebuild() {
        let err = sync_authenticated_user_id(&RpcContext::new()).expect_err("missing user");
        assert_eq!(err.code, ErrorCode::SyncFullRebuildRequired);
    }

    #[test]
    fn invalid_sync_entities_user_requires_full_rebuild() {
        let ctx = RpcContext::new().with_user_id("bad-user".to_string());
        let err = sync_authenticated_user_id(&ctx).expect_err("invalid user");
        assert_eq!(err.code, ErrorCode::SyncFullRebuildRequired);
    }

    #[test]
    fn missing_group_member_scope_requires_entity_resync() {
        let err = validate_group_member_scope(None).expect_err("expected scope failure");
        assert_eq!(err.code, ErrorCode::SyncEntityResyncRequired);
    }

    #[test]
    fn valid_group_member_scope_is_accepted() {
        validate_group_member_scope(Some("group:30001")).expect("group_id scope");
    }
}
