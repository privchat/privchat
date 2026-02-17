pub mod add;
pub mod leave;
pub mod list;
pub mod mute;
pub mod remove;
pub mod unmute;

use super::super::RpcServiceContext;

/// 注册 member 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = crate::rpc::GLOBAL_RPC_ROUTER.clone();

    router
        .register("group/member/add", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { add::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/remove", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { remove::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/list", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/leave", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { leave::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/mute", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { mute::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/unmute", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { unmute::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 member 模块路由注册完成 (add, remove, list, leave, mute, unmute)");
}
