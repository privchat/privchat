pub mod get;
pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use privchat_protocol::rpc::routes;

/// 注册个人资料模块的所有路由
pub async fn register_routes() {
    GLOBAL_RPC_ROUTER.register(routes::account_profile::GET, get::handle).await;
    GLOBAL_RPC_ROUTER.register(routes::account_profile::UPDATE, update::handle).await;
    
    tracing::debug!("📋 Profile 模块路由注册完成");
} 