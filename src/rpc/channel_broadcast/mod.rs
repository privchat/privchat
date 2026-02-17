pub mod channel;
pub mod content;

use super::RpcServiceContext;

/// 注册频道系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    channel::register_routes(services.clone()).await;
    content::register_routes(services.clone()).await;

    tracing::debug!("📋 Channel 系统路由注册完成");
}
