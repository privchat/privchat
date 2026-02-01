pub mod friend;
pub mod block;
pub mod blacklist;  // ✅ 新增黑名单模块

use super::RpcServiceContext;

/// 注册联系人系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    friend::register_routes(services.clone()).await;
    block::register_routes(services.clone()).await;
    blacklist::register_routes(services.clone()).await;  // ✅ 注册黑名单路由
    
    tracing::info!("📋 Contact 系统路由注册完成 (friend, block, blacklist)");
} 