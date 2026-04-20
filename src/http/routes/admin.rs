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

//! 管理 API 路由模块
//!
//! 统一的管理接口，使用 X-Service-Key 进行安全认证
//!
//! 包含以下管理功能：
//! - Token 管理：签发 token
//! - 用户管理：查询、更新、删除、封禁/解封用户
//! - 设备管理：查询设备、强制踢出设备
//! - 群组管理：查询、解散群组、成员管理
//! - 好友管理：查询好友关系
//! - 消息管控：查询消息、管理员撤回、发送系统消息
//! - 安全管控：Shadow Ban 管理、用户安全状态
//! - 在线状态：在线人数统计
//! - 系统运维：健康检查
//! - 登录日志：查询登录记录
//! - 统计报表：系统统计数据

use crate::auth::{IssueTokenRequest, IssueTokenResponse};
use crate::error::{Result, ServerError};
use crate::http::dto::admin as dto;
use crate::http::AdminServerState;
use axum::{
    extract::{ConnectInfo, Path, Query, State},
    http::HeaderMap,
    response::Json,
    routing::{delete, get, post, put},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use tracing::{debug, info, warn};

/// 创建管理 API 路由
pub fn create_route() -> Router<AdminServerState> {
    Router::new()
        // Token 管理（三方对接接口）
        .route("/api/admin/token/issue", post(issue_token))
        // 用户管理
        .route("/api/admin/users", post(create_user)) // 创建用户
        .route("/api/admin/users", get(list_users))
        .route("/api/admin/users/{user_id}", get(get_user))
        .route("/api/admin/users/{user_id}", put(update_user))
        .route("/api/admin/users/{user_id}", delete(delete_user))
        // 群组管理
        .route("/api/admin/groups", get(list_groups))
        .route("/api/admin/groups/{group_id}", get(get_group))
        .route("/api/admin/groups/{group_id}", delete(dissolve_group))
        // Room 频道管理
        .route("/api/admin/room", post(create_room_channel))
        .route("/api/admin/room", get(list_room_channels))
        .route("/api/admin/room/{channel_id}", get(get_room_channel))
        .route(
            "/api/admin/room/{channel_id}/broadcast",
            post(room_broadcast),
        )
        // 好友管理
        .route("/api/admin/friendships", post(create_friendship)) // 创建好友关系
        .route("/api/admin/friendships", get(list_friendships))
        // 登录日志
        .route("/api/admin/login-logs", get(list_login_logs))
        .route("/api/admin/login-logs/{log_id}", get(get_login_log))
        // 设备管理
        .route("/api/admin/devices", get(list_devices))
        .route("/api/admin/devices/{device_id}", get(get_device))
        // 统计报表
        .route("/api/admin/stats", get(get_stats))
        .route("/api/admin/stats/users", get(get_user_stats))
        .route("/api/admin/stats/groups", get(get_group_stats))
        .route("/api/admin/stats/messages", get(get_message_stats))
        // 聊天记录
        .route("/api/admin/messages", get(list_messages))
        .route("/api/admin/messages/{message_id}", get(get_message))
        // === P0: 用户封禁/解封 ===
        .route("/api/admin/users/{user_id}/suspend", post(suspend_user))
        .route("/api/admin/users/{user_id}/unsuspend", post(unsuspend_user))
        // === P0: 设备强制踢出 ===
        .route("/api/admin/devices/{device_id}/revoke", post(revoke_device))
        .route(
            "/api/admin/users/{user_id}/revoke-all-devices",
            post(revoke_all_user_devices),
        )
        // === P0: 群组成员管理 ===
        .route(
            "/api/admin/groups/{group_id}/members",
            get(list_group_members).post(add_group_member),
        )
        .route(
            "/api/admin/groups/{group_id}/members/{user_id}",
            delete(remove_group_member),
        )
        // === P0: 消息撤回 + 系统消息 ===
        .route(
            "/api/admin/messages/{message_id}/revoke",
            post(revoke_message),
        )
        .route("/api/admin/messages/send-system", post(send_system_message))
        .route("/api/admin/messages/send", post(send_message))
        // === P0: 安全管控 ===
        .route("/api/admin/security/shadow-banned", get(list_shadow_banned))
        .route(
            "/api/admin/security/shadow-ban/{user_id}",
            delete(unshadow_ban_user),
        )
        .route(
            "/api/admin/security/users/{user_id}/state",
            get(get_user_security_state),
        )
        .route(
            "/api/admin/security/users/{user_id}/reset",
            post(reset_user_security_state),
        )
        // === P0: 在线状态 ===
        .route("/api/admin/presence/online-count", get(get_online_count))
        .route("/api/admin/presence/users", get(list_online_users))
        .route(
            "/api/admin/presence/user/{user_id}",
            get(get_user_connection),
        )
        // === P1: 用户资源 ===
        .route("/api/admin/users/{user_id}/friends", get(get_user_friends))
        .route("/api/admin/users/{user_id}/devices", get(get_user_devices))
        .route("/api/admin/users/{user_id}/groups", get(get_user_groups))
        // === P1: 会话管理 ===
        .route(
            "/api/admin/users/{user_id}/channels",
            get(list_user_channels),
        )
        .route("/api/admin/channels/{channel_id}", get(get_channel))
        .route(
            "/api/admin/channels/{channel_id}/participants",
            get(list_channel_participants),
        )
        // === P1: 消息广播与搜索 ===
        .route("/api/admin/messages/broadcast", post(broadcast_message))
        .route("/api/admin/messages/search", get(search_messages))
        // === P0: 系统运维 ===
        .route("/api/admin/system/health", get(health_check))
}

// =====================================================
// 中间件：Service Key 验证
// =====================================================

/// 从请求头中提取并验证 Service Key
async fn verify_service_key(headers: &HeaderMap, state: &AdminServerState) -> Result<()> {
    let key = headers
        .get("X-Service-Key")
        .or_else(|| headers.get("x-service-key"))
        .ok_or_else(|| {
            warn!("缺少 X-Service-Key 请求头");
            ServerError::Unauthorized("缺少 X-Service-Key 请求头".to_string())
        })?;

    let service_key = key.to_str().map(|s| s.to_string()).map_err(|_| {
        warn!("X-Service-Key 格式无效");
        ServerError::Unauthorized("X-Service-Key 格式无效".to_string())
    })?;

    // 验证 service key
    if !state.service_key_manager.verify(&service_key).await {
        warn!(
            "❌ 无效的 service key: {}, 期望: {}",
            service_key,
            state.service_key_manager.display_expected().await
        );
        return Err(ServerError::Unauthorized("无效的 service key".to_string()));
    }

    Ok(())
}

// =====================================================
// Token 管理（三方对接接口）
// =====================================================

/// Token 签发接口
///
/// **定位：三方对接接口**
///
/// 此接口用于业务系统为用户签发 IM token，属于三方对接层面。
/// 业务系统通过此接口为已有用户生成 IM 登录凭证。
///
/// **重要说明**：
/// 1. `device_id` 是**可选**的：
///    - 如果客户端提供 `device_id`，必须是有效的 UUID 格式，服务器会使用此 `device_id`
///    - 如果客户端不提供，服务器会自动生成 UUID 作为 `device_id`
/// 2. 客户端在连接 WebSocket 时，`ConnectMessage.device_info.device_id` **必须与**返回的 `device_id` 一致
/// 3. JWT token 中已绑定 `device_id`，连接时验证会检查一致性，不匹配将拒绝连接
///
/// **注意**：如果需要管理员为用户签发 token（管理层面），可以添加新接口：
/// - `POST /api/admin/users/{user_id}/token` - 管理员为用户签发 token
///
/// POST /api/admin/token/issue
/// Headers: X-Service-Key: <service_key>
/// Body: IssueTokenRequest
///
/// 请求示例（客户端提供 device_id）：
/// ```json
/// {
///   "user_id": 12345,
///   "business_system_id": "ecommerce",
///   "device_id": "550e8400-e29b-41d4-a716-446655440000",  // 可选，必须是 UUID
///   "device_info": {
///     "app_id": "ios",
///     "device_name": "我的 iPhone",
///     "device_model": "iPhone 15 Pro",
///     "os_version": "iOS 17.2",
///     "app_version": "1.0.0"
///   },
///   "ttl": 604800  // 可选，默认 7 天
/// }
/// ```
///
/// 请求示例（服务器自动生成 device_id）：
/// ```json
/// {
///   "user_id": 12345,
///   "business_system_id": "ecommerce",
///   "device_info": {
///     "app_id": "ios",
///     "device_name": "我的 iPhone",
///     "device_model": "iPhone 15 Pro",
///     "os_version": "iOS 17.2",
///     "app_version": "1.0.0"
///   }
/// }
/// ```
///
/// 响应示例：
/// ```json
/// {
///   "im_token": "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9...",
///   "device_id": "550e8400-e29b-41d4-a716-446655440000",  // ⚠️ 客户端必须使用此 device_id
///   "expires_in": 604800,
///   "expires_at": "2026-02-01T12:00:00Z"
/// }
/// ```
async fn issue_token(
    State(state): State<AdminServerState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<IssueTokenRequest>,
) -> Result<Json<IssueTokenResponse>> {
    verify_service_key(&headers, &state).await?;

    debug!("收到 token 签发请求: user_id={}", request.user_id);

    let response = state
        .token_issue_service
        .issue_token(
            &extract_service_key(&headers)?,
            request,
            addr.ip().to_string(),
        )
        .await?;

    Ok(Json(response))
}

/// 从请求头中提取 Service Key（内部使用）
fn extract_service_key(headers: &HeaderMap) -> Result<String> {
    let key = headers
        .get("X-Service-Key")
        .or_else(|| headers.get("x-service-key"))
        .ok_or_else(|| ServerError::Unauthorized("缺少 X-Service-Key 请求头".to_string()))?;

    key.to_str()
        .map(|s| s.to_string())
        .map_err(|_| ServerError::Unauthorized("X-Service-Key 格式无效".to_string()))
}

// =====================================================
// 用户管理
// =====================================================

/// 创建用户请求
#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    /// 用户名（必填，唯一）
    username: String,
    /// 显示名称（可选）
    display_name: Option<String>,
    /// 邮箱（可选）
    email: Option<String>,
    /// 手机号（可选）
    phone: Option<String>,
    /// 头像URL（可选）
    avatar_url: Option<String>,
    /// 用户类型（可选，默认根据ID自动推断）
    user_type: Option<i16>,
}

/// 创建用户
///
/// POST /api/admin/users
///
/// 用于业务系统为已有用户创建IM账号
async fn create_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<CreateUserRequest>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let CreateUserRequest {
        username,
        display_name,
        email,
        phone,
        avatar_url,
        user_type,
    } = request;

    info!("创建用户: username={}", username);

    let created_user = state
        .user_service
        .create_user_admin(crate::service::CreateUserAdminParams {
            username,
            display_name,
            email,
            phone,
            avatar_url,
            user_type,
        })
        .await?;

    Ok(Json(json!({
        "success": true,
        "user_id": created_user.id,
        "username": created_user.username,
        "display_name": created_user.display_name,
        "email": created_user.email,
        "phone": created_user.phone,
        "avatar_url": created_user.avatar_url,
        "user_type": created_user.user_type,
        "created_at": created_user.created_at.timestamp_millis(),
        "message": "用户创建成功"
    })))
}

/// 用户列表查询参数
#[derive(Debug, Deserialize)]
struct UserListQuery {
    page: Option<u32>,
    page_size: Option<u32>,
    search: Option<String>,
    status: Option<i16>,
}

/// 获取用户列表
///
/// GET /api/admin/users?page=1&page_size=20&search=alice
async fn list_users(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<UserListQuery>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100); // 最大 100

    let (users, total) = if let Some(ref search) = params.search {
        let users = state.user_service.search(search).await?;
        let total = users.len() as u32;
        (users, total)
    } else {
        state
            .user_service
            .find_all_paginated(page, page_size)
            .await?
    };

    let user_list: Vec<Value> = users
        .into_iter()
        .map(|u| {
            json!({
                "user_id": u.id,
                "username": u.username,
                "display_name": u.display_name,
                "email": u.email,
                "phone": u.phone,
                "avatar_url": u.avatar_url,
                "user_type": u.user_type,
                "status": u.status.to_i16(),
                "created_at": u.created_at.timestamp_millis(),
                "last_active_at": u.last_active_at.map(|dt| dt.timestamp_millis()),
            })
        })
        .collect();

    Ok(Json(json!({
        "users": user_list,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取用户详情
///
/// GET /api/admin/users/:user_id
async fn get_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let user = state
        .user_service
        .find_by_id(user_id)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("用户 {} 不存在", user_id)))?;

    Ok(Json(json!({
        "user_id": user.id,
        "username": user.username,
        "display_name": user.display_name,
        "email": user.email,
        "phone": user.phone,
        "avatar_url": user.avatar_url,
        "user_type": user.user_type,
        "status": user.status,
        "privacy_settings": user.privacy_settings,
        "created_at": user.created_at.timestamp_millis(),
        "updated_at": user.updated_at.timestamp_millis(),
        "last_active_at": user.last_active_at.map(|dt| dt.timestamp_millis()),
    })))
}

/// 更新用户信息
///
/// PUT /api/admin/users/:user_id
#[derive(Debug, Deserialize)]
struct UpdateUserRequest {
    display_name: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    avatar_url: Option<String>,
    status: Option<i16>,
}

async fn update_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
    Json(request): Json<UpdateUserRequest>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let UpdateUserRequest {
        display_name,
        email,
        phone,
        avatar_url,
        status,
    } = request;

    state
        .user_service
        .update_user_admin(
            user_id,
            crate::service::UpdateUserAdminParams {
                display_name,
                email,
                phone,
                avatar_url,
                status,
            },
        )
        .await?;

    Ok(Json(json!({
        "success": true,
        "user_id": user_id,
        "message": "用户更新成功"
    })))
}

/// 删除/禁用用户
///
/// DELETE /api/admin/users/:user_id
async fn delete_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    state.user_service.delete_user_admin(user_id).await?;

    Ok(Json(json!({
        "success": true,
        "user_id": user_id,
        "message": "用户已删除"
    })))
}

// =====================================================
// 群组管理
// =====================================================

/// 获取群组列表
///
/// GET /api/admin/groups?page=1&page_size=20
async fn list_groups(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let page_size = params
        .get("page_size")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(20)
        .min(100);

    let (group_list, total) = state
        .channel_service
        .list_groups_admin(page, page_size)
        .await
        .map_err(|e| ServerError::Database(format!("查询群组列表失败: {}", e)))?;

    Ok(Json(json!({
        "groups": group_list,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取群组详情
///
/// GET /api/admin/groups/:group_id
async fn get_group(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(group_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let group = state
        .channel_service
        .get_group_admin(group_id)
        .await
        .map_err(|e| match e {
            ServerError::NotFound(msg) => ServerError::NotFound(msg),
            _ => ServerError::Database(format!("查询群组详情失败: {}", e)),
        })?;

    Ok(Json(group))
}

/// 解散群组
///
/// DELETE /api/admin/groups/:group_id
async fn dissolve_group(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(group_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    state
        .channel_service
        .dissolve_group_admin(group_id)
        .await
        .map_err(|e| match e {
            ServerError::NotFound(msg) => ServerError::NotFound(msg),
            _ => ServerError::Database(format!("解散群组失败: {}", e)),
        })?;

    Ok(Json(json!({
        "success": true,
        "group_id": group_id,
        "message": "群组已解散"
    })))
}

// =====================================================
// Room 频道管理
// =====================================================

use crate::infra::next_channel_id;

/// 创建 Room 频道
///
/// POST /api/admin/room
async fn create_room_channel(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(payload): Json<dto::CreateRoomChannelRequest>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let channel_id = next_channel_id();
    let name = payload
        .name
        .unwrap_or_else(|| format!("Room-{}", channel_id));

    info!(
        "📡 Admin: 创建 Room 频道 channel_id={}, name={}",
        channel_id, name
    );

    Ok(Json(json!({
        "success": true,
        "channel_id": channel_id,
        "name": name,
        "message": "频道创建成功"
    })))
}

/// 获取 Room 频道列表
///
/// GET /api/admin/room?page=1&page_size=20
async fn list_room_channels(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1)
        .max(1);
    let page_size = params
        .get("page_size")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(20)
        .min(100);

    let all_channels = state.subscribe_manager.get_all_channels();
    let total_channels = all_channels.len();
    let total_sessions = state.subscribe_manager.get_total_session_count();

    // 分页
    let start = ((page - 1) * page_size) as usize;
    let channels: Vec<Value> = all_channels
        .into_iter()
        .skip(start)
        .take(page_size as usize)
        .map(|(channel_id, session_count)| {
            json!({
                "channel_id": channel_id,
                "online_count": session_count,
            })
        })
        .collect();

    Ok(Json(json!({
        "channels": channels,
        "total": total_channels,
        "total_sessions": total_sessions,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取 Room 频道详情
///
/// GET /api/admin/room/:channel_id
async fn get_room_channel(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(channel_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let online_count = state.subscribe_manager.get_channel_online_count(channel_id);

    Ok(Json(json!({
        "channel_id": channel_id,
        "online_count": online_count,
    })))
}

/// Room 频道广播
///
/// POST /api/admin/room/:channel_id/broadcast
async fn room_broadcast(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(channel_id): Path<u64>,
    Json(payload): Json<dto::RoomBroadcastRequest>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let sessions = state.subscribe_manager.get_channel_sessions(channel_id);
    let online_count = sessions.len();

    let message_content = payload.content.clone();
    let publisher = payload.sender_id.map(|id| id.to_string());
    let server_msg_id = crate::infra::next_message_id();

    // Room 广播使用 PublishRequest（发布订阅协议）
    let publish_request = privchat_protocol::protocol::PublishRequest {
        channel_id,
        topic: None,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        payload: message_content.into_bytes(),
        publisher,
        server_message_id: Some(server_msg_id),
    };

    if let Err(e) = state
        .room_history_service
        .append_history(channel_id, &publish_request)
        .await
    {
        warn!(
            "⚠️ Room history append 失败 channel_id={}, server_msg_id={}, error={}",
            channel_id, server_msg_id, e
        );
    }

    if sessions.is_empty() {
        debug!(
            "📡 Room broadcast: channel_id={}, 无在线订阅者，仅写入历史",
            channel_id
        );
        return Ok(Json(json!({
            "success": true,
            "channel_id": channel_id,
            "online_count": 0,
            "delivered": 0,
            "server_message_id": server_msg_id,
            "message": "频道内无在线订阅者"
        })));
    }

    let transport = state.connection_manager.transport_server.read().await;
    let Some(server) = transport.as_ref() else {
        return Err(ServerError::Internal("TransportServer 未就绪".to_string()));
    };

    let payload_bytes = privchat_protocol::encode_message(&publish_request)
        .map_err(|e| ServerError::Protocol(format!("编码消息失败: {}", e)))?;

    // 并行广播：使用 tokio::spawn 并发发送，不逐个 await（非阻塞 fanout）
    let payload_bytes = std::sync::Arc::new(payload_bytes);
    let mut handles = Vec::with_capacity(sessions.len());

    for sid in sessions {
        let server_clone = server.clone();
        let bytes = payload_bytes.clone();
        handles.push(tokio::spawn(async move {
            let mut packet = msgtrans::packet::Packet::one_way(crate::infra::next_packet_id(), (*bytes).clone());
            packet.set_biz_type(privchat_protocol::protocol::MessageType::PublishRequest as u8);
            match server_clone.send_to_session(sid.clone(), packet).await {
                Ok(()) => {
                    debug!("📡 Room publish -> session {} 成功", sid);
                    true
                }
                Err(e) => {
                    warn!("📡 Room publish -> session {} 失败: {}", sid, e);
                    false
                }
            }
        }));
    }

    // 等待所有发送完成
    let mut delivered = 0usize;
    for handle in handles {
        if let Ok(true) = handle.await {
            delivered += 1;
        }
    }

    info!(
        "📡 Admin: Room 广播 channel_id={}, 在线={}, 投递={}",
        channel_id, online_count, delivered
    );

    Ok(Json(json!({
        "success": true,
        "channel_id": channel_id,
        "online_count": online_count,
        "delivered": delivered,
        "server_message_id": server_msg_id,
    })))
}

// =====================================================
// 好友管理
// =====================================================

/// 创建好友关系
///
/// POST /api/admin/friendships
///
/// 让两个用户成为好友，并自动创建私聊会话
async fn create_friendship(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<dto::CreateFriendshipRequest>,
) -> Result<Json<dto::CreateFriendshipResponse>> {
    verify_service_key(&headers, &state).await?;

    let user1_id = request.user1_id;
    let user2_id = request.user2_id;

    info!("创建好友关系: {} <-> {}", user1_id, user2_id);

    if !state.user_service.exists(user1_id).await? {
        return Err(ServerError::NotFound(format!("用户 {} 不存在", user1_id)));
    }
    if !state.user_service.exists(user2_id).await? {
        return Err(ServerError::NotFound(format!("用户 {} 不存在", user2_id)));
    }

    let channel_id = state
        .channel_service
        .create_friendship_admin(user1_id, user2_id)
        .await
        .map_err(|e| match e {
            ServerError::Validation(msg) => ServerError::Validation(msg),
            _ => ServerError::Database(format!("创建好友关系失败: {}", e)),
        })?;

    Ok(Json(dto::CreateFriendshipResponse {
        success: true,
        user1_id,
        user2_id,
        channel_id,
        message: "好友关系已创建，会话已生成".to_string(),
    }))
}

/// 获取好友关系列表
///
/// GET /api/admin/friendships?page=1&page_size=20
///
/// 注意：好友关系存储在内存中（FriendService），这里通过私聊会话推断好友关系
async fn list_friendships(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let page_size = params
        .get("page_size")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(20)
        .min(100);

    let (friendship_list, total) = state
        .channel_service
        .list_friendships_admin(page, page_size)
        .await
        .map_err(|e| ServerError::Database(format!("查询好友关系失败: {}", e)))?;

    Ok(Json(json!({
        "friendships": friendship_list,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取用户的好友列表
///
/// GET /api/admin/friendships/:user_id
///
/// 通过私聊会话推断用户的好友列表
async fn get_user_friends(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let friend_ids = state
        .channel_service
        .get_user_friends_admin(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户好友列表失败: {}", e)))?;

    let mut friends = Vec::new();
    for friend_id in friend_ids {
        // 获取好友用户信息
        if let Ok(Some(friend_user)) = state.user_service.find_by_id(friend_id).await {
            friends.push(json!({
                "user_id": friend_id,
                "username": friend_user.username,
                "display_name": friend_user.display_name,
                "avatar_url": friend_user.avatar_url,
            }));
        }
    }

    Ok(Json(json!({
        "user_id": user_id,
        "friends": friends,
        "total": friends.len(),
    })))
}

// =====================================================
// 用户群组
// =====================================================

/// 获取用户加入的群组列表
///
/// GET /api/admin/users/:user_id/groups
async fn get_user_groups(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let groups = state
        .channel_service
        .get_user_groups_admin(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户群组列表失败: {}", e)))?;

    Ok(Json(json!({
        "user_id": user_id,
        "groups": groups,
    })))
}

// =====================================================
// 登录日志
// =====================================================

/// 登录日志查询参数
#[derive(Debug, Deserialize)]
struct LoginLogQuery {
    user_id: Option<i64>,
    ip_address: Option<String>,
    status: Option<i16>,
    start_time: Option<i64>,
    end_time: Option<i64>,
    page: Option<i64>,
    page_size: Option<i64>,
}

/// 获取登录日志列表
///
/// GET /api/admin/login-logs?user_id=123&page=1&page_size=20
async fn list_login_logs(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<LoginLogQuery>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);
    let offset = (page - 1) * page_size;

    let query = crate::repository::LoginLogQuery {
        user_id: params.user_id,
        device_id: None,
        ip_address: params.ip_address,
        status: params.status,
        start_time: params.start_time,
        end_time: params.end_time,
        limit: Some(page_size),
        offset: Some(offset),
    };

    let logs = state
        .login_log_repository
        .get_user_logs(query)
        .await
        .map_err(|e| ServerError::Database(format!("查询登录日志失败: {}", e)))?;

    let log_list: Vec<Value> = logs
        .into_iter()
        .map(|log| {
            json!({
                "log_id": log.log_id,
                "user_id": log.user_id,
                "device_id": log.device_id.to_string(),
                "device_type": log.device_type,
                "device_name": log.device_name,
                "ip_address": log.ip_address,
                "status": log.status,
                "risk_score": log.risk_score,
                "is_new_device": log.is_new_device,
                "is_new_location": log.is_new_location,
                "created_at": log.created_at,
            })
        })
        .collect();

    Ok(Json(json!({
        "logs": log_list,
        "total": log_list.len(),
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取登录日志详情
///
/// GET /api/admin/login-logs/:log_id
async fn get_login_log(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(log_id): Path<i64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let log = state
        .login_log_repository
        .get_by_id(log_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询登录日志详情失败: {}", e)))?
        .ok_or_else(|| ServerError::NotFound(format!("登录日志 {} 不存在", log_id)))?;

    Ok(Json(json!({
        "log_id": log.log_id,
        "user_id": log.user_id,
        "device_id": log.device_id.to_string(),
        "token_jti": log.token_jti,
        "token_created_at": log.token_created_at,
        "token_first_used_at": log.token_first_used_at,
        "device_type": log.device_type,
        "device_name": log.device_name,
        "device_model": log.device_model,
        "os_version": log.os_version,
        "app_id": log.app_id,
        "app_version": log.app_version,
        "ip_address": log.ip_address,
        "user_agent": log.user_agent,
        "login_method": log.login_method,
        "auth_source": log.auth_source,
        "status": log.status,
        "risk_score": log.risk_score,
        "is_new_device": log.is_new_device,
        "is_new_location": log.is_new_location,
        "risk_factors": log.risk_factors,
        "notification_sent": log.notification_sent,
        "notification_method": log.notification_method,
        "notification_sent_at": log.notification_sent_at,
        "metadata": log.metadata,
        "created_at": log.created_at,
    })))
}

// =====================================================
// 设备管理
// =====================================================

/// 获取设备列表
///
/// GET /api/admin/devices?user_id=123&page=1&page_size=20
async fn list_devices(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params
        .get("page")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1);
    let page_size = params
        .get("page_size")
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(20)
        .min(100);
    let user_id_filter = params.get("user_id").and_then(|s| s.parse::<u64>().ok());

    let (devices, total) = state
        .device_manager_db
        .list_devices_admin(page, page_size, user_id_filter)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备列表失败: {}", e)))?;

    let device_list: Vec<Value> = devices
        .into_iter()
        .map(|d| {
            let device_type_str = match d.device_type {
                crate::auth::models::DeviceType::IOS => "ios",
                crate::auth::models::DeviceType::Android => "android",
                crate::auth::models::DeviceType::MacOS => "macos",
                crate::auth::models::DeviceType::Windows => "windows",
                crate::auth::models::DeviceType::Linux => "linux",
                crate::auth::models::DeviceType::Mobile => "mobile",
                crate::auth::models::DeviceType::Desktop => "desktop",
                crate::auth::models::DeviceType::Web => "web",
                crate::auth::models::DeviceType::Unknown => "unknown",
            };
            json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "device_model": d.device_model,
                "app_id": d.app_id,
                "device_type": device_type_str,
                "last_active_at": d.last_active_at.timestamp_millis(),
                "created_at": d.created_at.timestamp_millis(),
                "ip_address": d.ip_address,
            })
        })
        .collect();

    Ok(Json(json!({
        "devices": device_list,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取设备详情
///
/// GET /api/admin/devices/:device_id
async fn get_device(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let device = state
        .device_manager_db
        .get_device_admin(&device_id)
        .await
        .map_err(|e| match e {
            ServerError::NotFound(msg) => ServerError::NotFound(msg),
            _ => ServerError::Database(format!("查询设备详情失败: {}", e)),
        })?;

    let device_type_str = match device.device_type {
        crate::auth::models::DeviceType::IOS => "ios",
        crate::auth::models::DeviceType::Android => "android",
        crate::auth::models::DeviceType::MacOS => "macos",
        crate::auth::models::DeviceType::Windows => "windows",
        crate::auth::models::DeviceType::Linux => "linux",
        crate::auth::models::DeviceType::Mobile => "mobile",
        crate::auth::models::DeviceType::Desktop => "desktop",
        crate::auth::models::DeviceType::Web => "web",
        crate::auth::models::DeviceType::Unknown => "unknown",
    };

    Ok(Json(json!({
        "device_id": device.device_id,
        "device_name": device.device_name,
        "device_model": device.device_model,
        "app_id": device.app_id,
        "device_type": device_type_str,
        "last_active_at": device.last_active_at.timestamp_millis(),
        "created_at": device.created_at.timestamp_millis(),
        "ip_address": device.ip_address,
    })))
}

/// 获取用户的所有设备
///
/// GET /api/admin/devices/user/:user_id
async fn get_user_devices(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let devices = state
        .device_manager_db
        .get_user_devices(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户设备失败: {}", e)))?;

    let device_list: Vec<Value> = devices
        .into_iter()
        .map(|d| {
            let device_type_str = match d.device_type {
                crate::auth::models::DeviceType::IOS => "ios",
                crate::auth::models::DeviceType::Android => "android",
                crate::auth::models::DeviceType::MacOS => "macos",
                crate::auth::models::DeviceType::Windows => "windows",
                crate::auth::models::DeviceType::Linux => "linux",
                crate::auth::models::DeviceType::Mobile => "mobile",
                crate::auth::models::DeviceType::Desktop => "desktop",
                crate::auth::models::DeviceType::Web => "web",
                crate::auth::models::DeviceType::Unknown => "unknown",
            };
            json!({
                "device_id": d.device_id,
                "device_name": d.device_name,
                "device_model": d.device_model,
                "app_id": d.app_id,
                "device_type": device_type_str,
                "last_active_at": d.last_active_at.timestamp_millis(),
                "created_at": d.created_at.timestamp_millis(),
                "ip_address": d.ip_address,
            })
        })
        .collect();

    Ok(Json(json!({
        "user_id": user_id,
        "devices": device_list,
        "total": device_list.len(),
    })))
}

// =====================================================
// 统计报表
// =====================================================

/// 获取系统统计信息
///
/// GET /api/admin/stats
async fn get_stats(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    // 用户数
    let user_count = state.user_service.count().await?;

    // 群组数
    let group_stats = state
        .channel_service
        .get_group_stats_admin()
        .await
        .map_err(|e| ServerError::Database(format!("获取群组统计失败: {}", e)))?;
    let group_total = group_stats["total"].as_u64().unwrap_or(0) as usize;

    // 消息数
    let message_stats = state.message_service.admin_stats().await?;
    let message_total = message_stats["total"].as_u64().unwrap_or(0) as usize;

    // 设备数
    let (_, device_total) = state
        .device_manager_db
        .list_devices_admin(1, 1, None)
        .await
        .map_err(|e| ServerError::Database(format!("获取设备统计失败: {}", e)))?;

    Ok(Json(json!({
        "users": {
            "total": user_count,
        },
        "groups": {
            "total": group_total,
        },
        "messages": {
            "total": message_total,
        },
        "devices": {
            "total": device_total as usize,
        },
    })))
}

/// 获取用户统计
///
/// GET /api/admin/stats/users
async fn get_user_stats(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let total = state.user_service.count().await?;

    Ok(Json(json!({
        "total": total,
        "active": 0,  // TODO
        "inactive": 0,  // TODO
    })))
}

/// 获取群组统计
///
/// GET /api/admin/stats/groups
async fn get_group_stats(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let stats = state
        .channel_service
        .get_group_stats_admin()
        .await
        .map_err(|e| ServerError::Database(format!("获取群组统计失败: {}", e)))?;

    Ok(Json(stats))
}

/// 获取消息统计
///
/// GET /api/admin/stats/messages
async fn get_message_stats(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let stats = state.message_service.admin_stats().await?;

    Ok(Json(stats))
}

// =====================================================
// 聊天记录
// =====================================================

/// 消息查询参数
#[derive(Debug, Deserialize)]
struct MessageQuery {
    channel_id: Option<u64>,
    user_id: Option<u64>,
    start_time: Option<i64>,
    end_time: Option<i64>,
    page: Option<u32>,
    page_size: Option<u32>,
}

/// 获取消息列表
///
/// GET /api/admin/messages?channel_id=123&user_id=456&page=1&page_size=20
async fn list_messages(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<MessageQuery>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);

    let (message_list, total) = state
        .message_service
        .admin_list(
            params.channel_id,
            params.user_id,
            params.start_time,
            params.end_time,
            page,
            page_size,
        )
        .await?;

    Ok(Json(json!({
        "messages": message_list,
        "total": total,
        "page": page,
        "page_size": page_size,
    })))
}

/// 获取消息详情
///
/// GET /api/admin/messages/:message_id
async fn get_message(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(message_id): Path<u64>,
) -> Result<Json<Value>> {
    verify_service_key(&headers, &state).await?;

    let message = state
        .message_service
        .admin_get(message_id)
        .await?
        .ok_or_else(|| ServerError::NotFound(format!("消息 {} 不存在", message_id)))?;

    Ok(Json(message))
}

// =====================================================
// P0: 用户封禁/解封
// =====================================================

/// 封禁用户
///
/// POST /api/admin/users/:user_id/suspend
async fn suspend_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
    Json(request): Json<dto::SuspendUserRequest>,
) -> Result<Json<dto::SuspendUserResponse>> {
    verify_service_key(&headers, &state).await?;

    let reason = request.reason.as_deref().unwrap_or("admin_suspend");
    let result = state.admin_service.suspend_user(user_id, reason).await?;

    Ok(Json(dto::SuspendUserResponse {
        success: true,
        user_id,
        previous_status: result.previous_status,
        current_status: 2,
        reason: request.reason,
        revoked_devices: result.revoked_devices,
        message: "用户已封禁".to_string(),
    }))
}

/// 解封用户
///
/// POST /api/admin/users/:user_id/unsuspend
async fn unsuspend_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<dto::UnsuspendUserResponse>> {
    verify_service_key(&headers, &state).await?;

    let result = state.admin_service.unsuspend_user(user_id).await?;

    Ok(Json(dto::UnsuspendUserResponse {
        success: true,
        user_id,
        previous_status: result.previous_status,
        current_status: 0,
        message: "用户已解封".to_string(),
    }))
}

// =====================================================
// P0: 设备强制踢出
// =====================================================

/// 踢出指定设备
///
/// POST /api/admin/devices/:device_id/revoke
async fn revoke_device(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(device_id): Path<String>,
    Json(request): Json<dto::RevokeDeviceRequest>,
) -> Result<Json<dto::RevokeDeviceResponse>> {
    verify_service_key(&headers, &state).await?;

    let reason = request.reason.as_deref().unwrap_or("admin_revoke");
    state
        .admin_service
        .revoke_device(request.user_id, &device_id, reason)
        .await?;

    Ok(Json(dto::RevokeDeviceResponse {
        success: true,
        device_id,
        user_id: request.user_id,
        message: "设备已踢出".to_string(),
    }))
}

/// 撤销用户的全部设备
///
/// POST /api/admin/users/:user_id/revoke-all-devices
async fn revoke_all_user_devices(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
    Json(request): Json<dto::RevokeAllDevicesRequest>,
) -> Result<Json<dto::RevokeAllDevicesResponse>> {
    verify_service_key(&headers, &state).await?;

    let reason = request.reason.as_deref().unwrap_or("admin_revoke_all");
    let revoked_count = state
        .admin_service
        .revoke_all_devices(user_id, reason)
        .await?;

    Ok(Json(dto::RevokeAllDevicesResponse {
        success: true,
        user_id,
        revoked_count,
        message: format!("已撤销 {} 个设备", revoked_count),
    }))
}

// =====================================================
// P0: 群组成员管理
// =====================================================

/// 获取群组成员列表
///
/// GET /api/admin/groups/:group_id/members
async fn list_group_members(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(group_id): Path<u64>,
) -> Result<Json<dto::ListGroupMembersResponse>> {
    verify_service_key(&headers, &state).await?;

    let members = state.channel_service.list_members_admin(group_id).await?;

    let member_list: Vec<dto::GroupMemberItem> = members
        .into_iter()
        .map(|(uid, role, joined_at, nickname)| dto::GroupMemberItem {
            user_id: uid,
            role: role
                .map(|r| r.to_string())
                .unwrap_or_else(|| "member".to_string()),
            joined_at: Some(joined_at),
            nickname,
        })
        .collect();

    let total = member_list.len();

    Ok(Json(dto::ListGroupMembersResponse {
        group_id,
        members: member_list,
        total,
    }))
}

/// 移除群组成员
///
/// DELETE /api/admin/groups/:group_id/members/:user_id
async fn remove_group_member(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path((group_id, user_id)): Path<(u64, u64)>,
) -> Result<Json<dto::RemoveGroupMemberResponse>> {
    verify_service_key(&headers, &state).await?;

    state
        .channel_service
        .remove_member_admin(group_id, user_id)
        .await?;

    Ok(Json(dto::RemoveGroupMemberResponse {
        success: true,
        group_id,
        user_id,
        message: "成员已移除".to_string(),
    }))
}

// =====================================================
// P0: 消息撤回 + 系统消息
// =====================================================

/// 管理员撤回消息
///
/// POST /api/admin/messages/:message_id/revoke
///
/// admin 入口只做权限校验（`verify_service_key`）+ 委托到 `MessageService::revoke_message_admin`。
/// 跳过发送者/48h 限制，但共享 RPC 侧同一套副作用编排（事件/缓存/PTS/推送/离线清理）。
/// 见 `ADMIN_API_SPEC §1.4 Global Service Convergence Rule`。
async fn revoke_message(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(message_id): Path<u64>,
    Json(_request): Json<dto::RevokeMessageRequest>,
) -> Result<Json<dto::RevokeMessageResponse>> {
    verify_service_key(&headers, &state).await?;

    let summary = state
        .message_service
        .revoke_message_admin(message_id, crate::config::SYSTEM_USER_ID)
        .await?;

    Ok(Json(dto::RevokeMessageResponse {
        success: true,
        message_id: summary.message_id,
        channel_id: summary.channel_id,
        revoked_at: summary.revoked_at_ms,
        message: "消息已撤回".to_string(),
    }))
}

/// 发送系统消息
///
/// POST /api/admin/messages/send-system
async fn send_system_message(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<dto::SendSystemMessageRequest>,
) -> Result<Json<dto::SendSystemMessageResponse>> {
    verify_service_key(&headers, &state).await?;

    let message_type = parse_content_message_type(request.message_type.as_deref());

    let metadata = request
        .metadata
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let (channel_type, recipients) =
        resolve_channel_type_and_members(&state, request.channel_id).await?;

    let result = state
        .message_service
        .send_message(crate::service::ServerSendMessageRequest {
            channel_id: request.channel_id,
            sender_id: crate::config::SYSTEM_USER_ID,
            content: request.content.clone(),
            message_type,
            metadata,
            channel_type,
            recipient_user_ids: recipients,
        })
        .await
        .map_err(|e| ServerError::Internal(format!("发送系统消息失败: {}", e)))?;

    Ok(Json(dto::SendSystemMessageResponse {
        success: true,
        message_id: result.message_id,
        channel_id: request.channel_id,
        created_at: result.created_at,
        message: "系统消息已发送".to_string(),
    }))
}

// =====================================================
// P0: 安全管控
// =====================================================

/// 获取 Shadow Ban 用户列表
///
/// GET /api/admin/security/shadow-banned
async fn list_shadow_banned(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<dto::ListShadowBannedResponse>> {
    verify_service_key(&headers, &state).await?;

    let banned = state.security_service.list_shadow_banned().await;

    let users: Vec<dto::ShadowBannedItem> = banned
        .into_iter()
        .map(
            |(user_id, device_id, state, trust_score)| dto::ShadowBannedItem {
                user_id,
                device_id,
                state: format!("{:?}", state),
                trust_score,
            },
        )
        .collect();

    let total = users.len();

    Ok(Json(dto::ListShadowBannedResponse { users, total }))
}

/// 解除用户的 Shadow Ban
///
/// DELETE /api/admin/security/shadow-ban/:user_id
async fn unshadow_ban_user(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<dto::UnshadowBanResponse>> {
    verify_service_key(&headers, &state).await?;

    let devices = state
        .device_manager_db
        .get_user_devices(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户设备失败: {}", e)))?;

    let device_ids: Vec<String> = devices.iter().map(|d| d.device_id.clone()).collect();
    let affected = state
        .security_service
        .unban_all_user_devices(user_id, &device_ids)
        .await;

    Ok(Json(dto::UnshadowBanResponse {
        success: true,
        user_id,
        affected_devices: affected,
        message: "Shadow Ban 已解除".to_string(),
    }))
}

/// 获取用户安全状态
///
/// GET /api/admin/security/users/:user_id/state
async fn get_user_security_state(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<dto::UserSecurityStateResponse>> {
    verify_service_key(&headers, &state).await?;

    let devices = state
        .device_manager_db
        .get_user_devices(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户设备失败: {}", e)))?;

    let device_ids: Vec<String> = devices.iter().map(|d| d.device_id.clone()).collect();
    let device_states = state
        .security_service
        .get_user_device_states(user_id, &device_ids)
        .await;

    let devices_dto: Vec<dto::DeviceSecurityState> = device_states
        .into_iter()
        .map(
            |(device_id, client_state, trust_score)| dto::DeviceSecurityState {
                device_id,
                state: client_state
                    .map(|s| format!("{:?}", s))
                    .unwrap_or_else(|| "Unknown".to_string()),
                trust_score,
            },
        )
        .collect();

    Ok(Json(dto::UserSecurityStateResponse {
        user_id,
        devices: devices_dto,
    }))
}

/// 重置用户安全状态
///
/// POST /api/admin/security/users/:user_id/reset
async fn reset_user_security_state(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<dto::ResetSecurityStateResponse>> {
    verify_service_key(&headers, &state).await?;

    let devices = state
        .device_manager_db
        .get_user_devices(user_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户设备失败: {}", e)))?;

    let device_ids: Vec<String> = devices.iter().map(|d| d.device_id.clone()).collect();
    let affected = state
        .security_service
        .unban_all_user_devices(user_id, &device_ids)
        .await;

    Ok(Json(dto::ResetSecurityStateResponse {
        success: true,
        user_id,
        affected_devices: affected,
        message: "安全状态已重置".to_string(),
    }))
}

// =====================================================
// P0: 在线状态
// =====================================================

/// 获取在线连接数
///
/// GET /api/admin/presence/online-count
async fn get_online_count(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<dto::OnlineCountResponse>> {
    verify_service_key(&headers, &state).await?;

    let count = state.connection_manager.get_connection_count().await;

    Ok(Json(dto::OnlineCountResponse {
        online_count: count,
    }))
}

// =====================================================
// P0: 系统运维
// =====================================================

/// 健康检查
///
/// GET /api/admin/system/health
async fn health_check(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
) -> Result<Json<dto::HealthCheckResponse>> {
    verify_service_key(&headers, &state).await?;

    let connections = state.connection_manager.get_connection_count().await;

    Ok(Json(dto::HealthCheckResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: 0,
        connections,
    }))
}

// =====================================================
// 管理端发送消息（指定发送者）
// =====================================================

/// 管理端发送消息（可指定发送者）
///
/// POST /api/admin/messages/send
///
/// 与 send-system 不同，此接口允许指定任意 sender_id 作为消息发送者。
/// 用于业务系统让某个用户给另一个用户发消息的场景。
async fn send_message(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<dto::SendMessageRequest>,
) -> Result<Json<dto::SendMessageResponse>> {
    verify_service_key(&headers, &state).await?;

    let message_type = parse_content_message_type(request.message_type.as_deref());

    let content_len = request.content.chars().count();
    info!(
        "管理端发送消息: channel_id={}, sender_id={}, type={}, content_len={}",
        request.channel_id,
        request.sender_id,
        request.message_type.as_deref().unwrap_or("text"),
        content_len
    );

    let metadata = request
        .metadata
        .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    let (channel_type, recipients) =
        resolve_channel_type_and_members(&state, request.channel_id).await?;

    let result = state
        .message_service
        .send_message(crate::service::ServerSendMessageRequest {
            channel_id: request.channel_id,
            sender_id: request.sender_id,
            content: request.content.clone(),
            message_type,
            metadata,
            channel_type,
            recipient_user_ids: recipients,
        })
        .await
        .map_err(|e| {
            warn!(
                "管理端发送消息失败: channel_id={}, sender_id={}, error={}",
                request.channel_id, request.sender_id, e
            );
            ServerError::Internal(format!("发送消息失败: {}", e))
        })?;

    info!(
        "管理端发送消息成功: channel_id={}, sender_id={}, message_id={}, pts={}",
        request.channel_id, request.sender_id, result.message_id, result.pts
    );

    Ok(Json(dto::SendMessageResponse {
        success: true,
        message_id: result.message_id,
        channel_id: request.channel_id,
        sender_id: request.sender_id,
        created_at: result.created_at,
        message: "消息已发送".to_string(),
    }))
}

// =====================================================
// 管理端添加群成员
// =====================================================

/// 添加用户到群组
///
/// POST /api/admin/groups/:group_id/members
///
/// 自动添加用户到群组并发送系统公告 "[XXX加入了本群]"
async fn add_group_member(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(group_id): Path<u64>,
    Json(request): Json<dto::AddGroupMemberRequest>,
) -> Result<Json<dto::AddGroupMemberResponse>> {
    verify_service_key(&headers, &state).await?;

    let result = state
        .admin_service
        .add_user_to_group_with_announcement(group_id, request.user_id)
        .await?;

    Ok(Json(dto::AddGroupMemberResponse {
        success: true,
        group_id,
        user_id: request.user_id,
        announcement_message_id: result.announcement_message_id,
        message: "用户已加入群组".to_string(),
    }))
}

// =====================================================
// 在线用户列表
// =====================================================

/// 获取在线用户列表
///
/// GET /api/admin/presence/users
///
/// 返回当前所有在线用户及其设备信息
async fn list_online_users(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<dto::PageParams>,
) -> Result<Json<dto::ListOnlineUsersResponse>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);

    let all_connections: Vec<dto::OnlineDeviceItem> = state
        .connection_manager
        .get_all_connections()
        .await
        .into_iter()
        .map(|conn| dto::OnlineDeviceItem {
            device_id: conn.device_id,
            device_type: None,
            device_name: None,
            ip_address: None,
            connected_at: conn.connected_at,
            last_active: None,
        })
        .collect();

    let total_online = all_connections.len();
    let offset = ((page - 1) * page_size) as usize;
    let paged: Vec<dto::OnlineDeviceItem> = all_connections
        .into_iter()
        .skip(offset)
        .take(page_size as usize)
        .collect();

    Ok(Json(dto::ListOnlineUsersResponse {
        users: vec![], // TODO: 聚合用户信息
        total: total_online,
        page,
        page_size,
    }))
}

/// 获取指定用户的连接详情
///
/// GET /api/admin/presence/user/:user_id
async fn get_user_connection(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
) -> Result<Json<dto::UserConnectionResponse>> {
    verify_service_key(&headers, &state).await?;

    let connections = state.connection_manager.get_user_connections(user_id).await;

    let user_info = state
        .user_service
        .find_by_id(user_id)
        .await
        .ok()
        .flatten();

    Ok(Json(dto::UserConnectionResponse {
        user_id,
        username: user_info.as_ref().map(|u| u.username.clone()),
        nickname: user_info.as_ref().and_then(|u| u.display_name.clone()),
        online: !connections.is_empty(),
        devices: connections
            .into_iter()
            .map(|conn| dto::OnlineDeviceItem {
                device_id: conn.device_id,
                device_type: None,
                device_name: None,
                ip_address: None,
                connected_at: conn.connected_at,
                last_active: None,
            })
            .collect(),
    }))
}

// =====================================================
// 会话管理
// =====================================================

/// 获取会话列表
///
/// GET /api/admin/channels
/// 获取用户会话列表
///
/// GET /api/admin/users/:user_id/channels
async fn list_user_channels(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(user_id): Path<u64>,
    Query(params): Query<dto::PageParams>,
) -> Result<Json<dto::ListChannelsResponse>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);

    let result = state
        .channel_service
        .list_channels_admin(page, page_size, None, Some(user_id))
        .await
        .map_err(|e| ServerError::Database(format!("获取用户会话列表失败: {}", e)))?;

    let channels: Vec<dto::ChannelItem> = result
        .0
        .into_iter()
        .map(|v| dto::ChannelItem {
            channel_id: v
                .get("channel_id")
                .and_then(|v: &serde_json::Value| v.as_u64())
                .unwrap_or(0),
            channel_type: v
                .get("channel_type")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0) as i16,
            name: v
                .get("name")
                .and_then(|v: &serde_json::Value| v.as_str())
                .map(|s| s.to_string()),
            avatar_url: v
                .get("avatar_url")
                .and_then(|v: &serde_json::Value| v.as_str())
                .map(|s| s.to_string()),
            member_count: v
                .get("member_count")
                .and_then(|v: &serde_json::Value| v.as_u64())
                .map(|n| n as i32),
            last_message: None,
            created_at: v
                .get("created_at")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0),
        })
        .collect();

    Ok(Json(dto::ListChannelsResponse {
        channels,
        total: result.1 as usize,
        page,
        page_size,
    }))
}

/// 获取会话详情
///
/// GET /api/admin/channels/:channel_id
async fn get_channel(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(channel_id): Path<u64>,
) -> Result<Json<serde_json::Value>> {
    verify_service_key(&headers, &state).await?;

    let channel = state
        .channel_service
        .get_channel_admin(channel_id)
        .await
        .map_err(|e| ServerError::Database(format!("获取会话详情失败: {}", e)))?
        .ok_or_else(|| ServerError::NotFound(format!("会话 {} 不存在", channel_id)))?;

    Ok(Json(channel))
}

/// 获取会话参与者列表
///
/// GET /api/admin/channels/:channel_id/participants
async fn list_channel_participants(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Path(channel_id): Path<u64>,
    Query(params): Query<dto::PageParams>,
) -> Result<Json<serde_json::Value>> {
    verify_service_key(&headers, &state).await?;

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);

    let result = state
        .channel_service
        .list_participants_admin(channel_id, page, page_size)
        .await
        .map_err(|e| ServerError::Database(format!("获取参与者列表失败: {}", e)))?;

    Ok(Json(serde_json::json!({
        "channel_id": channel_id,
        "participants": result.0,
        "total": result.1,
    })))
}

// =====================================================
// 全局广播
// =====================================================

/// 全局广播消息
///
/// POST /api/admin/messages/broadcast
async fn broadcast_message(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<dto::BroadcastRequest>,
) -> Result<Json<dto::BroadcastResponse>> {
    verify_service_key(&headers, &state).await?;

    let target_scope = request.target_scope.unwrap_or_else(|| "all".to_string());
    let message_type = request.message_type.unwrap_or(5);

    let online_count = state.connection_manager.get_connection_count().await;

    info!(
        "开始全局广播, target_scope={}, online={}",
        target_scope, online_count
    );

    Ok(Json(dto::BroadcastResponse {
        success: true,
        target_scope,
        online_recipients: online_count,
        message: "广播已发送".to_string(),
    }))
}

// =====================================================
// 消息搜索
// =====================================================

/// 搜索消息
///
/// GET /api/admin/messages/search
async fn search_messages(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Query(params): Query<dto::SearchMessagesRequest>,
) -> Result<Json<dto::SearchMessagesResponse>> {
    verify_service_key(&headers, &state).await?;

    if params.keyword.trim().is_empty() {
        return Err(ServerError::Validation("关键词不能为空".to_string()));
    }

    let page = params.page.unwrap_or(1);
    let page_size = params.page_size.unwrap_or(20).min(100);

    let result = state
        .message_service
        .admin_search(
            &params.keyword,
            params.channel_id,
            params.user_id,
            params.message_type,
            params.start_time,
            params.end_time,
            page,
            page_size,
        )
        .await?;

    let messages: Vec<dto::SearchMessageItem> = result
        .0
        .into_iter()
        .map(|v| dto::SearchMessageItem {
            message_id: v
                .get("message_id")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0),
            channel_id: v
                .get("channel_id")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0),
            sender_id: v
                .get("sender_id")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0),
            content: v
                .get("content")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or("")
                .to_string(),
            message_type: v
                .get("message_type")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0) as i16,
            created_at: v
                .get("created_at")
                .and_then(|v: &serde_json::Value| v.as_i64())
                .unwrap_or(0),
        })
        .collect();

    Ok(Json(dto::SearchMessagesResponse {
        messages,
        total: result.1 as usize,
        page,
        page_size,
    }))
}

/// 解析消息类型字符串为 ContentMessageType
fn parse_content_message_type(s: Option<&str>) -> privchat_protocol::ContentMessageType {
    use privchat_protocol::ContentMessageType;
    match s {
        Some("image") => ContentMessageType::Image,
        // 普通音频文件作为 File 消息发送，不再有独立 Audio 消息类型
        Some("file") | Some("audio") => ContentMessageType::File,
        Some("voice") => ContentMessageType::Voice,
        Some("video") => ContentMessageType::Video,
        Some("system") => ContentMessageType::System,
        Some("location") => ContentMessageType::Location,
        Some("contact_card") => ContentMessageType::ContactCard,
        Some("sticker") => ContentMessageType::Sticker,
        Some("forward") => ContentMessageType::Forward,
        Some("link") => ContentMessageType::Link,
        _ => ContentMessageType::Text,
    }
}

/// 获取频道类型和成员 ID 列表（用于 Admin 主动发消息）
///
/// Direct 频道的权威成员源是 `privchat_channels.direct_user1_id / direct_user2_id`，
/// 不走 `members` HashMap（历史数据 participants 表可能缺行，SendMessageHandler
/// 对等路径也是直接读 direct_userX_id，此处对齐）。
/// Group / Room 才读 participants。
async fn resolve_channel_type_and_members(
    state: &AdminServerState,
    channel_id: u64,
) -> Result<(u8, Vec<u64>)> {
    use crate::model::channel::ChannelType;

    if let Some(channel) = state.channel_service.get_channel_opt(channel_id).await {
        let channel_type: u8 = match channel.channel_type {
            ChannelType::Direct => 1,
            ChannelType::Group => 2,
            ChannelType::Room => 2,
        };

        let members = if matches!(channel.channel_type, ChannelType::Direct) {
            let mut v = Vec::with_capacity(2);
            if let Some(u) = channel.direct_user1_id {
                v.push(u);
            }
            if let Some(u) = channel.direct_user2_id {
                if !v.contains(&u) {
                    v.push(u);
                }
            }
            v
        } else {
            channel.get_member_ids()
        };
        return Ok((channel_type, members));
    }

    // 内存里没有，从数据库查询参与者
    let participants = state
        .channel_service
        .get_channel_participants(channel_id)
        .await
        .map_err(|e| ServerError::Database(format!("查询频道参与者失败: {}", e)))?;

    let members: Vec<u64> = participants.iter().map(|p| p.user_id).collect();
    // 无法从参与者列表推断类型，保守按群组处理
    let channel_type: u8 = if members.len() <= 2 { 1 } else { 2 };
    Ok((channel_type, members))
}

