pub mod add;
pub mod list;
pub mod remove;
pub mod stats;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册 Reaction 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("message/reaction/add", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { add::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("message/reaction/remove", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { remove::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("message/reaction/list", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("message/reaction/stats", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { stats::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Reaction 模块路由注册完成 (add, remove, list, stats)");
}
