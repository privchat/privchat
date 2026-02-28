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

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::service::PrivacySettingsUpdate;
use privchat_protocol::rpc::account::privacy::AccountPrivacyUpdateRequest;
use serde_json::{json, Value};

/// 处理 更新隐私设置 请求
///
/// RPC: account/privacy/update
///
/// 请求参数：
/// ```json
/// {
///   "user_id": 1001,
///   "allow_add_by_group": true,                    // 可选
///   "allow_search_by_phone": true,                 // 可选
///   "allow_search_by_username": true,              // 可选
///   "allow_search_by_email": true,                 // 可选
///   "allow_search_by_qrcode": true,                // 可选
///   "allow_view_by_non_friend": false,             // 可选
///   "allow_receive_message_from_non_friend": true  // 可选（类似QQ/Telegram/Zalo，用于客服系统）
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "success": true,
///   "user_id": "alice",
///   "message": "隐私设置更新成功",
///   "updated_at": "2026-01-12T12:00:00Z"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 更新隐私设置 请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: AccountPrivacyUpdateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let user_id = request.user_id;

    // 构建更新对象（只更新提供的字段）
    let updates = PrivacySettingsUpdate {
        allow_add_by_group: request.allow_add_by_group,
        allow_search_by_phone: request.allow_search_by_phone,
        allow_search_by_username: request.allow_search_by_username,
        allow_search_by_email: request.allow_search_by_email,
        allow_search_by_qrcode: request.allow_search_by_qrcode,
        allow_view_by_non_friend: request.allow_view_by_non_friend,
        allow_receive_message_from_non_friend: request.allow_receive_message_from_non_friend,
    };

    // 更新隐私设置
    match services
        .privacy_service
        .update_privacy_settings(user_id, updates)
        .await
    {
        Ok(settings) => {
            tracing::debug!("✅ 隐私设置更新成功: user_id={}", user_id);
            Ok(json!({
                "success": true,
                "user_id": settings.user_id,
                "message": "隐私设置更新成功",
                "allow_add_by_group": settings.allow_add_by_group,
                "allow_search_by_phone": settings.allow_search_by_phone,
                "allow_search_by_username": settings.allow_search_by_username,
                "allow_search_by_email": settings.allow_search_by_email,
                "allow_search_by_qrcode": settings.allow_search_by_qrcode,
                "allow_view_by_non_friend": settings.allow_view_by_non_friend,
                "allow_receive_message_from_non_friend": settings.allow_receive_message_from_non_friend,
                "updated_at": settings.updated_at.to_rfc3339()
            }))
        }
        Err(e) => {
            tracing::error!("❌ 更新隐私设置失败: {}", e);
            Err(RpcError::internal(format!("更新隐私设置失败: {}", e)))
        }
    }
}
