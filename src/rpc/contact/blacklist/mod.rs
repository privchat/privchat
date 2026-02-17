pub mod add;
pub mod check;
pub mod list;
pub mod remove;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册黑名单模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    // ✨ 使用路由常量代替硬编码字符串
    router
        .register(routes::blacklist::ADD, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { add::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::blacklist::REMOVE, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { remove::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::blacklist::LIST, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::blacklist::CHECK, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { check::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Blacklist 模块路由注册完成 (使用 privchat-protocol routes)");
}
