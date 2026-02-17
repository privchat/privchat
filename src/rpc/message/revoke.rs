use chrono::Utc;
use privchat_protocol::protocol::{MessageSetting, PushMessageRequest};
use privchat_protocol::rpc::message::revoke::MessageRevokeRequest;
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::repository::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;

/// 消息撤回配置
const DEFAULT_REVOKE_TIME_LIMIT: i64 = 172800; // 48小时（与 Telegram 一致）

/// 处理消息撤回请求
///
/// 注意：
/// - 对于已收到消息的用户，会推送撤回事件（显示占位符）
/// - 对于未收到消息的用户，消息会被从离线队列中删除（不推送）
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理消息撤回请求: {:?}", body);

    // ✨ 使用协议层类型自动反序列化
    let mut request: MessageRevokeRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let message_id = request.server_message_id;
    let channel_id = request.channel_id;
    let user_id = request.user_id;

    tracing::debug!(
        "🔧 处理消息撤回请求: server_message_id={}, channel_id={}, user_id={}",
        message_id,
        channel_id,
        user_id
    );

    // 1. 从数据库查找消息
    let message = services
        .message_repository
        .as_ref()
        .find_by_id(message_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询消息失败: {}", e)))?
        .ok_or_else(|| RpcError::not_found("消息不存在".to_string()))?;

    // 2. 检查消息是否已撤回
    if message.revoked {
        return Err(RpcError::validation("消息已被撤回".to_string()));
    }

    // 3. 验证权限：只有发送者或群管理员可以撤回
    let is_sender = message.sender_id == user_id;
    let is_admin = if let Some(channel) =
        services.channel_service.get_channel(&channel_id).await.ok()
    {
        if let Some(member) = channel.members.get(&user_id) {
            matches!(
                member.role,
                crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
            )
        } else {
            false
        }
    } else {
        false
    };

    if !is_sender && !is_admin {
        return Err(RpcError::forbidden(
            "只有发送者或群管理员可以撤回消息".to_string(),
        ));
    }

    // 4. 检查时间限制（普通用户48小时，管理员无限制）
    if is_sender && !is_admin {
        let now = Utc::now().timestamp();
        let message_time = message.created_at.timestamp();
        if now - message_time > DEFAULT_REVOKE_TIME_LIMIT {
            return Err(RpcError::validation(format!(
                "消息发送已超过 {} 小时，无法撤回",
                DEFAULT_REVOKE_TIME_LIMIT / 3600
            )));
        }
    }

    // 5. 在数据库中标记消息为已撤回
    services
        .message_repository
        .as_ref()
        .revoke_message(message_id, user_id)
        .await
        .map_err(|e| RpcError::internal(format!("撤回消息失败: {}", e)))?;

    tracing::debug!(
        "✅ 消息已在数据库中标记为撤回: message_id={}, revoked_by={}",
        message_id,
        user_id
    );

    // ✨ Phase 3: 发布 MessageRevoked 事件（用于撤销推送）
    if let Some(event_bus) = crate::handler::send_message_handler::get_global_event_bus() {
        let event = crate::domain::events::DomainEvent::MessageRevoked {
            message_id,
            conversation_id: channel_id,
            revoker_id: user_id,
            timestamp: Utc::now().timestamp(),
        };

        if let Err(e) = event_bus.publish(event) {
            warn!("⚠️ 发布 MessageRevoked 事件失败: {}", e);
        } else {
            tracing::debug!("✅ MessageRevoked 事件已发布: message_id={}", message_id);
        }
    }

    // 6. 同时更新内存缓存
    if let Err(e) = services
        .message_history_service
        .revoke_message(&channel_id, &message_id)
        .await
    {
        warn!("⚠️ 更新内存缓存失败: {}", e);
    }

    // 7. 推送撤回事件给所有参与者
    if let Err(e) = distribute_revoke_event(&services, channel_id, message_id, user_id).await {
        warn!("⚠️ 推送撤回事件失败: {}，但消息已撤回", e);
    }

    // 8. 从离线队列中删除该消息，确保未收到消息的用户不会再收到
    let participants = services
        .channel_service
        .get_channel_participants(channel_id)
        .await
        .map_err(|e| RpcError::internal(format!("获取会话参与者失败: {}", e)))?;

    for participant in participants {
        if let Err(e) = services
            .offline_queue_service
            .remove_message_by_id(participant.user_id, message_id)
            .await
        {
            warn!(
                "⚠️ 从用户 {} 的离线队列中删除消息 {} 失败: {}",
                participant.user_id, message_id, e
            );
        } else {
            tracing::debug!(
                "🗑️ 成功从用户 {} 的离线队列中删除消息 {}",
                participant.user_id,
                message_id
            );
        }
    }

    tracing::debug!("✅ 消息撤回完成: message_id={}", message_id);

    // 简单操作，返回 true
    Ok(json!(true))
}

/// 分发撤回事件给所有参与者
async fn distribute_revoke_event(
    services: &RpcServiceContext,
    channel_id: u64,
    message_id: u64,
    revoked_by: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as u64;

    // 构造撤回事件
    let event_payload = json!({
        "message_id": message_id,
        "channel_id": channel_id,
        "revoked_by": revoked_by,
        "revoked_at": now,
    });

    let revoke_event = PushMessageRequest {
        setting: MessageSetting {
            need_receipt: false,
            signal: 0,
        },
        msg_key: format!("revoke_{}", message_id),
        server_message_id: message_id,
        message_seq: 0,
        local_message_id: 0,
        stream_no: String::new(),
        stream_seq: 0,
        stream_flag: 0,
        timestamp: (now / 1000) as u32,
        channel_id,
        channel_type: 1,
        message_type: privchat_protocol::ContentMessageType::System.as_u32(),
        expire: 0,
        topic: "message.revoke".to_string(),
        from_uid: revoked_by,
        payload: event_payload.to_string().into_bytes(),
    };

    // 获取会话参与者
    let participants = services
        .channel_service
        .get_channel_participants(channel_id)
        .await?;

    // 推送给所有参与者
    for participant in participants {
        if let Err(e) = services
            .message_router
            .route_message_to_user(&participant.user_id, revoke_event.clone())
            .await
        {
            warn!("⚠️ 推送撤回事件给用户 {} 失败: {}", participant.user_id, e);
        } else {
            tracing::debug!("📤 成功推送撤回事件给用户 {}", participant.user_id);
        }
    }

    Ok(())
}
