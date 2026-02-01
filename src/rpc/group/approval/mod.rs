pub mod list;
pub mod handle;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册审批模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router.register("group/approval/list", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { list::handle(body, services, ctx).await })
        })
    }).await;

    router.register("group/approval/handle", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { handle::handle(body, services, ctx).await })
        })
    }).await;

    tracing::debug!("📋 Approval 模块路由注册完成 (list, handle)");
}

