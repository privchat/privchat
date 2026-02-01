/// Phase 8: 同步相关 RPC 处理器
/// 
/// RPC 路由：
/// - sync/submit - 客户端提交命令
/// - sync/get_difference - 获取差异
/// - sync/get_channel_pts - 获取频道 pts
/// - sync/batch_get_channel_pts - 批量获取频道 pts

use crate::rpc::router::GLOBAL_RPC_ROUTER;
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::routes;
use tracing::{info, error};

// Phase 8 RPC handlers 在本文件中实现

use serde_json::Value;
use crate::rpc::error::{RpcError, RpcResult};

/// 注册同步系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // sync/get_channel_pts - 获取频道 pts
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::sync::GET_CHANNEL_PTS, move |body, _ctx| {
        let services = services_clone.clone();
        async move {
            handle_get_channel_pts_rpc(body, services).await
        }
    }).await;
    
    // sync/get_difference - 获取差异
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::sync::GET_DIFFERENCE, move |body, _ctx| {
        let services = services_clone.clone();
        async move {
            handle_get_difference_rpc(body, services).await
        }
    }).await;
    
    // sync/submit - 客户端提交命令
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::sync::SUBMIT, move |body, ctx| {
        let services = services_clone.clone();
        async move {
            handle_submit_rpc(body, services, ctx).await
        }
    }).await;
    
    // sync/batch_get_channel_pts - 批量获取频道 pts
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::sync::BATCH_GET_CHANNEL_PTS, move |body, _ctx| {
        let services = services_clone.clone();
        async move {
            handle_batch_get_channel_pts_rpc(body, services).await
        }
    }).await;
    
    info!("📋 Sync 系统路由注册完成 (get_channel_pts, get_difference, submit, batch_get_channel_pts)");
}

/// RPC 处理函数：获取频道 pts
async fn handle_get_channel_pts_rpc(body: Value, services: RpcServiceContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::{GetChannelPtsRequest, GetChannelPtsResponse};
    
    let request: GetChannelPtsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    
    // 直接从 pts_generator 获取当前 pts
    let current_pts = services.pts_generator
        .current_pts(request.channel_id, request.channel_type)
        .await;
    
    let response = GetChannelPtsResponse {
        current_pts,
    };
    
    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：获取差异
async fn handle_get_difference_rpc(body: Value, services: RpcServiceContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::GetDifferenceRequest;
    
    let request: GetDifferenceRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    
    info!(
        "收到差异拉取请求: channel_id={}, channel_type={}, last_pts={}, limit={:?}",
        request.channel_id, request.channel_type, request.last_pts, request.limit
    );
    
    // 使用 SyncService 处理差异拉取
    let response = services.sync_service.handle_get_difference(request).await
        .map_err(|e| {
            error!("SyncService.handle_get_difference 失败: {}", e);
            RpcError::internal(format!("获取差异失败: {}", e))
        })?;
    
    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：客户端提交命令
async fn handle_submit_rpc(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::ClientSubmitRequest;
    
    let request: ClientSubmitRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    
    // 保存需要的字段（在 request 被移动之前）
    let channel_id = request.channel_id;
    
    // 获取当前用户ID
    let sender_id = crate::rpc::get_current_user_id(&ctx)?;
    
    // 使用 SyncService 处理客户端提交
    let response = services.sync_service.handle_client_submit(request, sender_id).await
        .map_err(|e| {
            error!("SyncService.handle_client_submit 失败: {}", e);
            RpcError::internal(format!("提交失败: {}", e))
        })?;
    
    info!(
        "✅ sync/submit 成功: local_message_id={}, channel_id={}, pts={:?}, has_gap={}",
        response.local_message_id, channel_id, response.pts, response.has_gap
    );
    
    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// RPC 处理函数：批量获取频道 pts
async fn handle_batch_get_channel_pts_rpc(body: Value, services: RpcServiceContext) -> RpcResult<Value> {
    use privchat_protocol::rpc::sync::BatchGetChannelPtsRequest;
    
    let request: BatchGetChannelPtsRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数错误: {}", e)))?;
    
    // 使用 SyncService 处理批量获取 pts
    let response = services.sync_service.handle_batch_get_channel_pts(request).await
        .map_err(|e| {
            error!("SyncService.handle_batch_get_channel_pts 失败: {}", e);
            RpcError::internal(format!("批量获取 pts 失败: {}", e))
        })?;
    
    info!("✅ sync/batch_get_channel_pts 成功: 返回 {} 个频道的 pts", response.channel_pts_map.len());
    
    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
