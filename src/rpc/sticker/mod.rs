//! 表情包系统 RPC 接口

pub mod package;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册表情包系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 表情包库列表
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(
            "sticker/package/list",
            move |params, _ctx| {
                let services = services_clone.clone();
                Box::pin(async move { package::list::handle(services, params).await })
            },
        )
        .await;
    
    // 表情包库详情
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(
            "sticker/package/detail",
            move |params, _ctx| {
                let services = services_clone.clone();
                Box::pin(async move { package::detail::handle(services, params).await })
            },
        )
        .await;
    
    tracing::info!("📦 Sticker 系统路由注册完成");
}

