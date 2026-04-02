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

use crate::repository::MessageRepository;
/// Phase 8: 同步相关 RPC 处理器
///
/// RPC 路由：
/// - sync/submit - 客户端提交命令
/// - sync/get_difference - 获取差异
/// - sync/get_channel_pts - 获取频道 pts
/// - sync/batch_get_channel_pts - 批量获取频道 pts
use crate::rpc::router::GLOBAL_RPC_ROUTER;
use crate::rpc::RpcServiceContext;
use chrono::{DateTime, Utc};
use msgtrans::SessionId;
use privchat_protocol::rpc::routes;
use tracing::{error, warn};

// Phase 8 RPC handlers 在本文件中实现

use crate::rpc::error::{RpcError, RpcResult};
use serde_json::Value;

fn sync_authenticated_user_id(ctx: &crate::rpc::RpcContext) -> RpcResult<u64> {
    let user_id_str = ctx.user_id.as_ref().ok_or_else(|| {
        RpcError::from_code(
            privchat_protocol::ErrorCode::SyncFullRebuildRequired,
            "session user missing for sync/session_ready".to_string(),
        )
    })?;

    user_id_str.parse::<u64>().map_err(|_| {
        RpcError::from_code(
            privchat_protocol::ErrorCode::SyncFullRebuildRequired,
            "invalid session user_id for sync/session_ready".to_string(),
        )
    })
}

/// 注册同步系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // sync/get_channel_pts - 获取频道 pts
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::sync::GET_CHANNEL_PTS, move |body, _ctx| {
            let services = services_clone.clone();
            async move { handle_get_channel_pts_rpc(body, services).await }
        })
        .await;

    // sync/get_difference - 获取差异
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::sync::GET_DIFFERENCE, move |body, _ctx| {
            let services = services_clone.clone();
            async move { handle_get_difference_rpc(body, services).await }
        })
        .await;

    // sync/submit - 客户端提交命令
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::sync::SUBMIT, move |body, ctx| {
            let services = services_clone.clone();
            async move { handle_submit_rpc(body, services, ctx).await }
        })
        .await;

    // sync/batch_get_channel_pts - 批量获取频道 pts
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::sync::BATCH_GET_CHANNEL_PTS, move |body, _ctx| {
            let services = services_clone.clone();
            async move { handle_batch_get_channel_pts_rpc(body, services).await }
        })
        .await;

    // sync/session_ready - 客户端完成 bootstrap，打开补差+实时闸门
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::sync::SESSION_READY, move |body, ctx| {
            let services = services_clone.clone();
            async move { handle_session_ready_rpc(body, services, ctx).await }
        })
        .await;

    tracing::debug!("📋 Sync 系统路由注册完成 (get_channel_pts, get_difference, submit, batch_get_channel_pts, session_ready)");
}

/// RPC 处理函数：获取频道 pts
async fn handle_get_channel_pts_rpc(body: Value, services: RpcServiceContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::GetChannelPtsRequest;

    let request: GetChannelPtsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;

    let response = services
        .sync_service
        .handle_get_channel_pts(request)
        .await
        .map_err(|e| {
            error!("SyncService.handle_get_channel_pts 失败: {}", e);
            RpcError::from(e)
        })?;

    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：获取差异
async fn handle_get_difference_rpc(body: Value, services: RpcServiceContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::GetDifferenceRequest;

    let request: GetDifferenceRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;

    tracing::debug!(
        "收到差异拉取请求: channel_id={}, channel_type={}, last_pts={}, limit={:?}",
        request.channel_id,
        request.channel_type,
        request.last_pts,
        request.limit
    );

    // 使用 SyncService 处理差异拉取
    let response = services
        .sync_service
        .handle_get_difference(request)
        .await
        .map_err(|e| {
            error!("SyncService.handle_get_difference 失败: {}", e);
            RpcError::from(e)
        })?;

    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：客户端提交命令
async fn handle_submit_rpc(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::ClientSubmitRequest;

    let request: ClientSubmitRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    let request_for_projection = request.clone();

    // 保存需要的字段（在 request 被移动之前）
    let channel_id = request.channel_id;

    // 获取当前用户ID
    let sender_id = crate::rpc::get_current_user_id(&ctx)?;

    // 使用 SyncService 处理客户端提交
    let response = services
        .sync_service
        .handle_client_submit(request, sender_id)
        .await
        .map_err(|e| {
            error!("SyncService.handle_client_submit 失败: {}", e);
            RpcError::internal(format!("提交失败: {}", e))
        })?;

    if let Err(e) =
        project_submit_to_message_views(&services, &request_for_projection, sender_id, &response)
            .await
    {
        warn!(
            "sync/submit 投影到消息视图失败: channel_id={}, local_message_id={}, error={}",
            request_for_projection.channel_id, request_for_projection.local_message_id, e
        );
    }

    tracing::debug!(
        "✅ sync/submit 成功: local_message_id={}, channel_id={}, pts={:?}, has_gap={}",
        response.local_message_id,
        channel_id,
        response.pts,
        response.has_gap
    );

    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：批量获取频道 pts
async fn handle_batch_get_channel_pts_rpc(
    body: Value,
    services: RpcServiceContext,
) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::BatchGetChannelPtsRequest;

    let request: BatchGetChannelPtsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;

    // 使用 SyncService 处理批量获取 pts
    let response = services
        .sync_service
        .handle_batch_get_channel_pts(request)
        .await
        .map_err(|e| {
            error!("SyncService.handle_batch_get_channel_pts 失败: {}", e);
            RpcError::internal(format!("批量获取 pts 失败: {}", e))
        })?;

    tracing::debug!(
        "✅ sync/batch_get_channel_pts 成功: 返回 {} 个频道的 pts",
        response.channel_pts_map.len()
    );

    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：会话 READY（幂等）
async fn handle_session_ready_rpc(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::{SessionReadyRequest, SessionReadyResponse};

    // 兼容 {} / null 空请求
    if !body.is_null() {
        let _: SessionReadyRequest = serde_json::from_value(body)
            .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    }

    let user_id = sync_authenticated_user_id(&ctx)?;
    let session_id_str = ctx
        .session_id
        .as_ref()
        .ok_or_else(|| {
            RpcError::from_code(
                privchat_protocol::ErrorCode::SyncFullRebuildRequired,
                "missing session_id for sync/session_ready".to_string(),
            )
        })?;
    let session_id = parse_session_id(session_id_str)?;

    let transitioned = services
        .auth_session_manager
        .mark_ready_for_push(&session_id)
        .await;

    if transitioned {
        tracing::info!(
            "✅ sync/session_ready: session={} user={} 首次 READY，触发补差推送",
            session_id,
            user_id
        );
        services.offline_worker.trigger_push(user_id).await;
    } else {
        tracing::info!(
            "ℹ️ sync/session_ready: session={} user={} 重复 READY（幂等）",
            session_id,
            user_id
        );
    }

    let response: SessionReadyResponse = true;
    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

fn parse_session_id(session_id_str: &str) -> RpcResult<SessionId> {
    let raw = session_id_str
        .strip_prefix("session-")
        .unwrap_or(session_id_str);
    let id = raw
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("无效 session_id: {}", session_id_str)))?;
    Ok(SessionId::from(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_session_ready_user_requires_full_rebuild() {
        let err = sync_authenticated_user_id(&crate::rpc::RpcContext::new()).expect_err("missing");
        assert_eq!(err.code, privchat_protocol::ErrorCode::SyncFullRebuildRequired);
    }

    #[test]
    fn invalid_session_ready_user_requires_full_rebuild() {
        let ctx = crate::rpc::RpcContext::new().with_user_id("not-a-u64".to_string());
        let err = sync_authenticated_user_id(&ctx).expect_err("invalid");
        assert_eq!(err.code, privchat_protocol::ErrorCode::SyncFullRebuildRequired);
    }
}

async fn project_submit_to_message_views(
    services: &RpcServiceContext,
    request: &privchat_protocol::rpc::sync::ClientSubmitRequest,
    sender_id: u64,
    response: &privchat_protocol::rpc::sync::ClientSubmitResponse,
) -> Result<(), String> {
    if !matches!(
        response.decision,
        privchat_protocol::rpc::sync::ServerDecision::Accepted
    ) {
        return Ok(());
    }
    let Some(server_msg_id) = response.server_msg_id else {
        return Ok(());
    };
    let Some(pts) = response.pts else {
        return Ok(());
    };

    let existing = services
        .message_repository
        .find_by_id(server_msg_id)
        .await
        .map_err(|e| format!("query message by id failed: {e}"))?;
    if existing.is_some() {
        return Ok(());
    }

    let (message_type, content, metadata) = normalize_submit_payload(request);
    let server_ts =
        DateTime::<Utc>::from_timestamp_millis(response.server_timestamp).unwrap_or_else(Utc::now);
    let msg = crate::model::message::Message {
        message_id: server_msg_id,
        channel_id: request.channel_id,
        sender_id,
        pts: Some(pts as i64),
        local_message_id: Some(request.local_message_id),
        content: content.clone(),
        message_type,
        metadata,
        reply_to_message_id: None,
        created_at: server_ts,
        updated_at: server_ts,
        deleted: false,
        deleted_at: None,
        revoked: false,
        revoked_at: None,
        revoked_by: None,
    };

    services
        .message_repository
        .create(&msg)
        .await
        .map_err(|e| format!("create message failed: {e}"))?;

    let _ = services
        .channel_service
        .update_last_message(request.channel_id, server_msg_id)
        .await;
    // channel last preview is derived on client from local message table.

    Ok(())
}

fn normalize_submit_payload(
    request: &privchat_protocol::rpc::sync::ClientSubmitRequest,
) -> (
    privchat_protocol::ContentMessageType,
    String,
    serde_json::Value,
) {
    let cmd = request.command_type.to_lowercase();
    let msg_type = match cmd.as_str() {
        "image" => privchat_protocol::ContentMessageType::Image,
        "file" => privchat_protocol::ContentMessageType::File,
        "voice" => privchat_protocol::ContentMessageType::Voice,
        "video" => privchat_protocol::ContentMessageType::Video,
        "audio" => privchat_protocol::ContentMessageType::Audio,
        "location" => privchat_protocol::ContentMessageType::Location,
        "contact_card" => privchat_protocol::ContentMessageType::ContactCard,
        "sticker" => privchat_protocol::ContentMessageType::Sticker,
        "forward" => privchat_protocol::ContentMessageType::Forward,
        "system" => privchat_protocol::ContentMessageType::System,
        _ => privchat_protocol::ContentMessageType::Text,
    };

    let content = request
        .payload
        .get("text")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .or_else(|| {
            request
                .payload
                .get("content")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| match msg_type {
            privchat_protocol::ContentMessageType::Location => "[location]".to_string(),
            privchat_protocol::ContentMessageType::ContactCard => "[contact_card]".to_string(),
            _ => request.payload.to_string(),
        });

    (msg_type, content, request.payload.clone())
}
