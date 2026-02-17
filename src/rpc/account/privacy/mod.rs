pub mod get;
pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册隐私设置模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("account/privacy/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { get::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("account/privacy/update", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { update::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Privacy 模块路由注册完成 (get, update)");
}
