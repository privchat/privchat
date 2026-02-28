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

use crate::model::QRType;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 解析 QR 码 请求
///
/// RPC: qrcode/resolve
///
/// 请求参数：
/// ```json
/// {
///   "qr_key": "7a8b9c0d1e2f",
///   "scanner_id": "bob",
///   "token": "xyz"              // 可选：群组邀请时需要
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "qr_type": "user",
///   "target_id": "alice",
///   "action": "show_profile",   // 建议的操作
///   "data": {                   // 详细信息（根据类型不同而不同）
///     "user_id": "alice",
///     "nickname": "Alice",
///     "avatar": "..."
///   },
///   "used_count": 5
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 解析 QR 码 请求: {:?}", body);

    // 解析参数
    let qr_key = body
        .get("qr_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("qr_key is required".to_string()))?;

    // 从 ctx 获取当前用户 ID
    let scanner_id = crate::rpc::get_current_user_id(&ctx)?;

    let token = body.get("token").and_then(|v| v.as_str());

    // 解析 QR Key
    let record = services
        .qrcode_service
        .resolve(qr_key, scanner_id, token)
        .await
        .map_err(|e| match e {
            crate::error::ServerError::NotFound(_) => RpcError::not_found(format!("{}", e)),
            crate::error::ServerError::BadRequest(_) => RpcError::validation(format!("{}", e)),
            crate::error::ServerError::Unauthorized(_) => RpcError::unauthorized(format!("{}", e)),
            _ => RpcError::internal(format!("解析 QR 码失败: {}", e)),
        })?;

    // 根据类型获取详细信息和建议的操作
    let (data, action) = match record.qr_type {
        QRType::User => {
            // 获取用户信息
            let user_data = json!({
                "user_id": record.target_id,
                // TODO: 从 UserService 获取用户详情
                "message": "请调用 user/profile/get 获取完整用户信息"
            });
            (user_data, "show_profile")
        }

        QRType::Group => {
            // 解析群组ID
            let group_id = record.target_id.parse::<u64>().map_err(|_| {
                RpcError::validation(format!("Invalid group_id in QR code: {}", record.target_id))
            })?;

            // 获取群组信息
            let group = services
                .channel_service
                .get_channel(&group_id)
                .await
                .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

            let group_data = json!({
                "group_id": group.id,
                "name": group.metadata.name,
                "description": group.metadata.description,
                "avatar": group.metadata.avatar_url,
                "member_count": group.members.len(),
                "settings": {
                    "join_need_approval": group.settings.as_ref().map(|s| s.require_approval).unwrap_or(false),
                    "member_can_invite": group.settings.as_ref().map(|s| s.allow_member_invite).unwrap_or(false),
                }
            });
            (group_data, "show_group")
        }

        QRType::Auth => {
            // 扫码登录
            let auth_data = json!({
                "session_id": record.target_id,
                "message": "请调用 auth/confirm 确认登录"
            });
            (auth_data, "confirm_login")
        }

        QRType::Feature => {
            // 其他功能
            let feature_data = json!({
                "feature_id": record.target_id,
                "metadata": record.metadata
            });
            (feature_data, "handle_feature")
        }
    };

    tracing::debug!(
        "✅ QR 码解析成功: qr_key={}, type={}, target={}, scanner={}",
        qr_key,
        record.qr_type.as_str(),
        record.target_id,
        scanner_id
    );

    Ok(json!({
        "qr_type": record.qr_type.as_str(),
        "target_id": record.target_id,
        "action": action,
        "data": data,
        "used_count": record.used_count,
        "max_usage": record.max_usage,
        "expire_at": record.expire_at.map(|t| t.to_rfc3339()),
    }))
}
