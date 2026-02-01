use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::member::GroupMemberMuteRequest;

/// 处理 禁言成员 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 禁言成员 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupMemberMuteRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 operator_id
    request.operator_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let group_id = request.group_id;
    let operator_id = request.operator_id;
    let user_id = request.user_id;
    let mute_duration = request.mute_duration;
    
    // 计算禁言到期时间
    let mute_until = if mute_duration > 0 {
        Some(chrono::Utc::now() + chrono::Duration::seconds(mute_duration as i64))
    } else {
        // 0 表示永久禁言
        Some(chrono::Utc::now() + chrono::Duration::days(365 * 100))
    };
    
    // 调用 Channel 服务禁言成员
    match services.channel_service.set_member_muted(
        &group_id,
        &user_id,
        true,
        mute_until,
    ).await {
        Ok(()) => {
            tracing::info!("✅ 成功禁言成员: group={}, user={}, duration={}秒", 
                group_id, user_id, mute_duration);
            // 返回禁言到期时间戳（毫秒），0 表示永久禁言
            let muted_until_ts = if mute_duration > 0 {
                (chrono::Utc::now() + chrono::Duration::seconds(mute_duration as i64)).timestamp_millis() as u64
            } else {
                0  // 永久禁言
            };
            Ok(json!(muted_until_ts))
        }
        Err(e) => {
            tracing::error!("❌ 禁言成员失败: group={}, user={}, error={}", 
                group_id, user_id, e);
            Err(RpcError::internal(format!("禁言成员失败: {}", e)))
        }
    }
}
