use serde_json::Value;
use tracing::{debug, warn};

use crate::rpc::{RpcContext, RpcServiceContext, RpcResult, RpcError, get_current_user_id};
use privchat_protocol::presence::*;
use privchat_protocol::PushMessageRequest;

/// RPC Handler: presence/typing
/// 
/// 发送输入状态通知
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
    
    debug!(
        "📥 presence/typing: user {} in channel {} is_typing={} action={:?}",
        user_id, req.channel_id, req.is_typing, req.action_type
    );
    
    // 3. 构造通知消息
    let notification = TypingStatusNotification {
        user_id,
        username: None, // 可选：从缓存或数据库获取用户名
        channel_id: req.channel_id,
        channel_type: req.channel_type,
        is_typing: req.is_typing,
        action_type: req.action_type.clone(),
        timestamp: chrono::Utc::now().timestamp(),
    };
    
    // 4. 广播给会话中的其他成员
    let notification_payload = serde_json::to_vec(&notification)
        .map_err(|e| RpcError::internal(format!("Serialize notification failed: {}", e)))?;
    
    // 根据会话类型决定广播方式
    match req.channel_type {
        1 => {
            // 私聊：channel_id 就是对方的 user_id
            // 推送给对方（如果对方在线）
            let target_user_id = req.channel_id;
            
            // 构造 PushMessageRequest
            let push_msg = PushMessageRequest {
                setting: Default::default(),
                msg_key: String::new(),
                server_message_id: 0, // 输入状态不需要消息ID
                message_seq: 0,
                local_message_id: 0,
                stream_no: String::new(),
                stream_seq: 0,
                stream_flag: 0,
                timestamp: chrono::Utc::now().timestamp() as u32,
                channel_id: req.channel_id,
                channel_type: 1, // 私聊
                message_type: privchat_protocol::ContentMessageType::System.as_u32(),
                expire: 0,
                topic: String::new(),
                from_uid: user_id,
                payload: notification_payload,
            };
            
            // 使用 MessageRouter 推送
            if let Err(e) = services.message_router
                .route_message_to_user(&target_user_id, push_msg)
                .await
            {
                warn!("Failed to broadcast typing to user {}: {}", target_user_id, e);
            } else {
                debug!("✅ Broadcasted typing status to user {}", target_user_id);
            }
        }
        2 => {
            // 群聊：需要查询群成员并广播给其他在线成员
            // TODO: 实现群聊广播逻辑
            // 1. 查询群成员列表
            // 2. 过滤掉当前用户
            // 3. 推送给所有在线成员
            
            debug!("TODO: Broadcast typing status to group {}", req.channel_id);
        }
        _ => {
            warn!("Unknown channel_type: {}", req.channel_type);
        }
    }
    
    debug!(
        "📢 Broadcasting typing status to channel {} (type={})",
        req.channel_id, req.channel_type
    );
    
    // 5. 返回响应
    let response = TypingIndicatorResponse {
        code: 0,
        message: "OK".to_string(),
    };
    
    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("Serialize response failed: {}", e)))
}

