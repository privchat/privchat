use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::PushMessageRequest;
use serde_json::{json, Value};
use tracing::{error, warn};

/// 处理 标记已读 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 标记已读 请求: {:?}", body);

    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("channel_id is required (must be u64)".to_string()))?;

    let message_id = body
        .get("message_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("message_id is required (must be u64)".to_string()))?;

    // 1. 更新会话已读状态（UserChannelView）
    match services
        .channel_service
        .mark_as_read(&user_id, &channel_id, message_id)
        .await
    {
        Ok(_) => {
            tracing::debug!(
                "✅ 会话已读状态已更新: {} 在频道 {} 消息 {}",
                user_id,
                channel_id,
                message_id
            );
        }
        Err(e) => {
            warn!("⚠️ 更新会话已读状态失败: {}", e);
            // 继续执行，不返回错误（因为可能会话不存在，但已读回执仍应记录）
        }
    }

    // 2. 记录已读回执（ReadReceipt）
    let receipt = match services
        .read_receipt_service
        .record_read_receipt(message_id, channel_id, user_id)
        .await
    {
        Ok(receipt) => {
            tracing::debug!(
                "✅ 消息已读回执已记录: {} 在频道 {} 消息 {} (已读时间: {})",
                user_id,
                channel_id,
                message_id,
                receipt.read_at
            );
            receipt
        }
        Err(e) => {
            error!("❌ 记录已读回执失败: {}", e);
            return Err(RpcError::internal(format!("记录已读回执失败: {}", e)));
        }
    };

    // 3. ✅ 新增：广播已读通知给发送者
    if let Err(e) =
        broadcast_read_receipt(&services, message_id, channel_id, user_id, &receipt.read_at).await
    {
        warn!("⚠️ 广播已读通知失败: {}，但已读回执已记录", e);
        // 广播失败不影响主流程
    }

    // Protocol contract: message/status/read response is boolean.
    // Keep response payload strictly typed as `true` on success.
    let _ = receipt; // receipt is kept for side effects above (store + broadcast).
    Ok(json!(true))
}

/// 广播已读通知给消息发送者
async fn broadcast_read_receipt(
    services: &RpcServiceContext,
    message_id: u64,
    channel_id: u64,
    reader_id: u64,
    read_at: &chrono::DateTime<chrono::Utc>,
) -> Result<(), String> {
    // 1. 获取消息信息（找到发送者）
    // 优化：如果消息不存在，优雅处理，不报错（已读回执已记录）
    let message = match services
        .message_history_service
        .get_message(&message_id)
        .await
    {
        Ok(msg) => msg,
        Err(e) => {
            // 消息不存在时，不广播已读通知（可能是历史消息或已删除的消息）
            tracing::debug!(
                "消息不存在，跳过已读通知广播: message_id={}, error={}",
                message_id,
                e
            );
            return Ok(()); // 优雅返回，不报错
        }
    };

    // 2. 获取频道信息（判断频道类型）
    // 优化：如果频道不存在，优雅处理
    let channel = match services.channel_service.get_channel(&channel_id).await {
        Ok(ch) => ch,
        Err(e) => {
            tracing::debug!(
                "频道不存在，跳过已读通知广播: channel_id={}, error={}",
                channel_id,
                e
            );
            return Ok(()); // 优雅返回，不报错
        }
    };

    // 3. 根据频道类型决定广播策略
    use crate::model::channel::ChannelKind;
    let channel_kind: ChannelKind = channel.channel_type.clone().into();
    match channel_kind {
        ChannelKind::PrivateChat => {
            // 私聊：直接通知发送者
            tracing::debug!(
                "📨 准备广播已读通知：私聊消息 {} 已被 {} 读取，通知发送者 {}",
                message_id,
                reader_id,
                message.sender_id
            );

            // 构造已读通知消息
            let notification_payload = json!({
                "message_type": "notification",
                "content": "消息已读",
                "metadata": {
                    "notification_type": "read_receipt",
                    "message_id": message_id.to_string(),
                    "channel_id": channel_id,
                    "reader_id": reader_id.to_string(),
                    "read_at": read_at.to_rfc3339(),
                }
            });

            let notification = PushMessageRequest {
                setting: Default::default(),
                msg_key: String::new(),
                server_message_id: message_id,
                message_seq: 0,
                local_message_id: 0,
                stream_no: String::new(),
                stream_seq: 0,
                stream_flag: 0,
                timestamp: chrono::Utc::now().timestamp() as u32,
                channel_id: channel_id,
                channel_type: 1, // DirectMessage
                message_type: privchat_protocol::ContentMessageType::System.as_u32(),
                expire: 0,
                topic: String::new(),
                from_uid: 0, // system
                payload: serde_json::to_vec(&notification_payload)
                    .map_err(|e| format!("序列化通知失败: {}", e))?,
            };

            // 发送给消息发送者
            services
                .message_router
                .route_message_to_user(&message.sender_id, notification)
                .await
                .map_err(|e| format!("发送已读通知失败: {}", e))?;

            tracing::debug!("✅ 已读通知已发送给发送者 {}", message.sender_id);
        }
        ChannelKind::GroupChat => {
            // 群聊：不主动广播，发送者可通过 read_list/read_stats RPC 查询
            tracing::debug!(
                "📊 群聊消息 {} 已被 {} 标记已读，发送者可主动查询已读列表",
                message_id,
                reader_id
            );
            // 可选：推送已读计数更新（暂不实现，减少服务器负载）
        }
        _ => {
            // 其他类型频道暂不处理
            tracing::debug!("⚠️ 频道类型 {:?} 暂不支持已读回执广播", channel_kind);
        }
    }

    Ok(())
}
