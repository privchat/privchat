pub mod get;
pub mod mute_all;
pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册群设置模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("group/settings/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { get::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/settings/update", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { update::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/settings/mute_all", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { mute_all::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Settings 模块路由注册完成 (get, update, mute_all)");
}
