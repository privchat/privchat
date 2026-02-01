use axum::{
    extract::State,
    http::StatusCode,
    Json,
};
use serde_json::{json, Value};
use tracing::{debug, error, info};

use crate::error::ServerError;
use crate::state::AppState;
use privchat_protocol::rpc::presence::*;
use privchat_protocol::presence::*;

/// RPC Handler: presence/subscribe
/// 
/// 订阅用户的在线状态（打开私聊会话时调用）
/// 
/// # 功能
/// 1. 建立订阅关系
/// 2. 返回目标用户的当前在线状态
/// 
/// # 参数
/// - target_user_id: 要订阅的用户ID
/// 
/// # 返回
/// - current_status: 目标用户的当前在线状态
pub async fn handle_subscribe_presence(
    State(state): State<AppState>,
    user_id: u64,
    params: Value,
) -> Result<Json<Value>, ServerError> {
    // 1. 解析请求参数
    let req: SubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| ServerError::BadRequest(format!("Invalid params: {}", e)))?;
    
    debug!(
        "📥 presence/subscribe: user {} subscribing to user {}",
        user_id, req.target_user_id
    );
    
    // 2. 验证参数
    if req.target_user_id == 0 {
        return Err(ServerError::BadRequest("target_user_id cannot be 0".to_string()));
    }
    
    if user_id == req.target_user_id {
        return Err(ServerError::BadRequest("Cannot subscribe to yourself".to_string()));
    }
    
    // 3. 执行订阅
    let current_status = state.presence_manager
        .subscribe(user_id, req.target_user_id)
        .await?;
    
    // 4. 返回响应
    let response = SubscribePresenceResponse {
        code: 0,
        message: "OK".to_string(),
        current_status,
    };
    
    info!(
        "✅ User {} subscribed to user {} (status: {:?})",
        user_id, req.target_user_id, response.current_status.status
    );
    
    Ok(Json(serde_json::to_value(response)?))
}

/// RPC Handler: presence/unsubscribe
/// 
/// 取消订阅用户的在线状态（关闭私聊会话时调用）
/// 
/// # 功能
/// 1. 移除订阅关系
/// 
/// # 参数
/// - target_user_id: 要取消订阅的用户ID
pub async fn handle_unsubscribe_presence(
    State(state): State<AppState>,
    user_id: u64,
    params: Value,
) -> Result<Json<Value>, ServerError> {
    // 1. 解析请求参数
    let req: UnsubscribePresenceRequest = serde_json::from_value(params)
        .map_err(|e| ServerError::BadRequest(format!("Invalid params: {}", e)))?;
    
    debug!(
        "📥 presence/unsubscribe: user {} unsubscribing from user {}",
        user_id, req.target_user_id
    );
    
    // 2. 执行取消订阅
    state.presence_manager.unsubscribe(user_id, req.target_user_id);
    
    // 3. 返回响应
    let response = UnsubscribePresenceResponse {
        code: 0,
        message: "OK".to_string(),
    };
    
    info!(
        "✅ User {} unsubscribed from user {}",
        user_id, req.target_user_id
    );
    
    Ok(Json(serde_json::to_value(response)?))
}

/// RPC Handler: presence/typing
/// 
/// 发送输入状态通知
/// 
/// # 功能
/// 1. 接收用户的输入状态
/// 2. 广播给会话中的其他成员
/// 
/// # 参数
/// - channel_id: 会话ID
/// - channel_type: 会话类型（1=私聊，2=群聊）
/// - is_typing: 是否正在输入
/// - action_type: 输入动作类型
pub async fn handle_typing_indicator(
    State(state): State<AppState>,
    user_id: u64,
    params: Value,
) -> Result<Json<Value>, ServerError> {
    // 1. 解析请求参数
    let req: TypingIndicatorRequest = serde_json::from_value(params)
        .map_err(|e| ServerError::BadRequest(format!("Invalid params: {}", e)))?;
    
    debug!(
        "📥 presence/typing: user {} in channel {} is_typing={} action={:?}",
        user_id, req.channel_id, req.is_typing, req.action_type
    );
    
    // 2. 构造通知消息
    let notification = TypingStatusNotification {
        user_id,
        username: None, // TODO: 从缓存或数据库获取用户名
        channel_id: req.channel_id,
        channel_type: req.channel_type,
        is_typing: req.is_typing,
        action_type: req.action_type.clone(),
        timestamp: chrono::Utc::now().timestamp(),
    };
    
    // 3. TODO: 广播给会话中的其他成员
    // 这里需要：
    // - 如果是私聊（channel_type=1），发给对方
    // - 如果是群聊（channel_type=2），发给群里的其他在线成员
    // - 使用 PublishRequest 或直接推送
    
    debug!(
        "📢 Broadcasting typing status to channel {} (type={})",
        req.channel_id, req.channel_type
    );
    
    // 4. 返回响应
    let response = TypingIndicatorResponse {
        code: 0,
        message: "OK".to_string(),
    };
    
    Ok(Json(serde_json::to_value(response)?))
}

/// RPC Handler: presence/status/get
/// 
/// 批量获取用户的在线状态（用于好友列表等场景）
/// 
/// # 功能
/// 1. 批量查询多个用户的在线状态
/// 
/// # 参数
/// - user_ids: 要查询的用户ID列表
pub async fn handle_get_online_status(
    State(state): State<AppState>,
    _user_id: u64,
    params: Value,
) -> Result<Json<Value>, ServerError> {
    // 1. 解析请求参数
    let req: GetOnlineStatusRequest = serde_json::from_value(params)
        .map_err(|e| ServerError::BadRequest(format!("Invalid params: {}", e)))?;
    
    debug!("📥 presence/status/get: querying {} users", req.user_ids.len());
    
    // 2. 验证参数
    if req.user_ids.is_empty() {
        return Err(ServerError::BadRequest("user_ids cannot be empty".to_string()));
    }
    
    if req.user_ids.len() > 100 {
        return Err(ServerError::BadRequest("Cannot query more than 100 users at once".to_string()));
    }
    
    // 3. 批量查询
    let statuses = state.presence_manager
        .batch_get_status(req.user_ids)
        .await;
    
    // 4. 返回响应
    let response = GetOnlineStatusResponse {
        code: 0,
        message: "OK".to_string(),
        statuses,
    };
    
    info!(
        "✅ Returned online status for {} users",
        response.statuses.len()
    );
    
    Ok(Json(serde_json::to_value(response)?))
}

/// RPC Handler: presence/stats
/// 
/// 获取在线状态系统的统计信息（管理员功能）
pub async fn handle_get_presence_stats(
    State(state): State<AppState>,
) -> Result<Json<Value>, ServerError> {
    let stats = state.presence_manager.get_stats();
    
    info!("📊 Presence stats: {:?}", stats);
    
    Ok(Json(json!({
        "code": 0,
        "message": "OK",
        "data": stats
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::PresenceManager;
    use std::sync::Arc;
    
    fn create_test_state() -> AppState {
        let presence_manager = Arc::new(PresenceManager::with_default_config());
        
        AppState {
            presence_manager,
            // ... 其他字段根据实际 AppState 定义填充
        }
    }
    
    #[tokio::test]
    async fn test_subscribe_presence() {
        let state = create_test_state();
        let user_id = 1;
        let params = json!({
            "target_user_id": 2
        });
        
        let result = handle_subscribe_presence(
            State(state),
            user_id,
            params,
        ).await;
        
        assert!(result.is_ok());
    }
    
    #[tokio::test]
    async fn test_subscribe_self_should_fail() {
        let state = create_test_state();
        let user_id = 1;
        let params = json!({
            "target_user_id": 1
        });
        
        let result = handle_subscribe_presence(
            State(state),
            user_id,
            params,
        ).await;
        
        assert!(result.is_err());
    }
}
