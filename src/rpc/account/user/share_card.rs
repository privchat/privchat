use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::account::user::AccountUserShareCardRequest;

/// 处理 分享名片 请求
/// 
/// 好友可以分享其他好友的名片给其他人
/// 规则：
/// 1. 只有好友可以分享名片
/// 2. 分享给别人后，不能再分享给其他人（只能一层）
/// 3. 每个分享记录只能使用一次
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理分享名片请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: AccountUserShareCardRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 sharer_id
    request.sharer_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let sharer_id = request.sharer_id;
    let target_user_id = request.target_user_id;
    let receiver_id = request.receiver_id;
    
    // 1. 验证分享者是否是被分享用户的好友
    if !services.friend_service.is_friend(sharer_id, target_user_id).await {
        return Err(RpcError::forbidden("Only friends can share cards".to_string()));
    }
    
    // 2. 验证分享者是否已经分享过（只能一层）
    // TODO: 实现 has_shared_card 方法
    // if services.cache_manager.has_shared_card(sharer_id, target_user_id).await {
    //     return Err(RpcError::forbidden("Already shared, cannot share again".to_string()));
    // }
    
    // 3. 创建分享记录
    match services.cache_manager.create_card_share(sharer_id, target_user_id, receiver_id).await {
        Ok(share_record) => {
            tracing::info!("✅ 名片分享成功: {} -> {} (via {})", sharer_id, receiver_id, target_user_id);
            Ok(json!({
                "share_id": share_record.share_id,
                "target_user_id": target_user_id,
                "receiver_id": receiver_id,
                "created_at": share_record.created_at.to_rfc3339(),
            }))
        }
        Err(e) => {
            tracing::error!("❌ 创建名片分享记录失败: {}", e);
            Err(RpcError::internal(format!("Failed to create card share: {}", e)))
        }
    }
}

