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

use serde_json::Value;
use tracing::warn;

use crate::rpc::{get_current_user_id, RpcContext, RpcError, RpcResult, RpcServiceContext};
use privchat_protocol::presence::*;

/// RPC Handler: presence/typing
///
/// 发送输入状态通知（通过 Subscribe/Publish 机制广播给频道订阅者）
pub async fn handle(
    params: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    // 1. 获取当前用户ID
    let user_id = get_current_user_id(&ctx)?;

    // 2. 解析请求参数
    let req: TypingIndicatorRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("Invalid params: {}", e)))?;

    tracing::debug!(
        "📥 presence/typing: user {} in channel {} is_typing={} action={:?}",
        user_id,
        req.channel_id,
        req.is_typing,
        req.action_type
    );

    // 2.5 限频检查：(user_id, channel_id) 500ms 内只允许 1 次广播
    if !services.typing_rate_limiter.check_and_update(user_id, req.channel_id) {
        tracing::debug!(
            "⏱️ presence/typing: rate limited user {} in channel {} (dropped_total={})",
            user_id,
            req.channel_id,
            services.typing_rate_limiter.dropped_total(),
        );
        // 被限频时仍返回成功（客户端无需感知）
        let response = TypingIndicatorResponse {
            code: 0,
            message: "OK".to_string(),
        };
        return serde_json::to_value(response)
            .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)));
    }

    // 3. 构造通知消息
    let notification = TypingStatusNotification {
        user_id,
        username: None,
        channel_id: req.channel_id,
        channel_type: req.channel_type,
        is_typing: req.is_typing,
        action_type: req.action_type.clone(),
        timestamp: chrono::Utc::now().timestamp(),
    };

    let notification_payload = serde_json::to_vec(&notification)
        .map_err(|e| RpcError::internal(format!("Serialize notification failed: {}", e)))?;

    // 4. 构造 PublishRequest (topic="typing")
    let publish_request = privchat_protocol::protocol::PublishRequest {
        channel_id: req.channel_id,
        topic: Some("typing".to_string()),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        payload: notification_payload,
        publisher: Some(user_id.to_string()),
        server_message_id: None, // typing 事件不需要消息ID
    };

    let payload_bytes = privchat_protocol::encode_message(&publish_request)
        .map_err(|e| RpcError::internal(format!("Encode PublishRequest failed: {}", e)))?;

    // 5. 获取该频道的所有订阅者并广播
    let sessions = services.subscribe_manager.get_channel_sessions(req.channel_id);
    let sender_session_id = ctx.session_id.clone();

    let transport = services.connection_manager.transport_server.read().await;
    if let Some(server) = transport.as_ref() {
        let mut delivered = 0usize;
        for sid in &sessions {
            // 跳过发送者自己
            if sender_session_id.as_deref() == Some(&sid.to_string()) {
                continue;
            }

            let mut packet = msgtrans::packet::Packet::one_way(0u32, payload_bytes.clone());
            packet.set_biz_type(
                privchat_protocol::protocol::MessageType::PublishRequest as u8,
            );

            // best-effort：发送失败仅记录日志
            match server.send_to_session(sid.clone(), packet).await {
                Ok(_) => {
                    delivered += 1;
                }
                Err(e) => {
                    warn!("Failed to send typing to session {}: {}", sid, e);
                }
            }
        }

        tracing::debug!(
            "📢 Broadcast typing to {}/{} subscribers for channel {}",
            delivered,
            sessions.len().saturating_sub(1),
            req.channel_id,
        );
    }

    // 6. 返回响应
    let response = TypingIndicatorResponse {
        code: 0,
        message: "OK".to_string(),
    };

    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)))
}
