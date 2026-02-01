pub mod transfer_owner;
pub mod set;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册角色管理模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router.register("group/role/transfer_owner", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { transfer_owner::handle(body, services, ctx).await })
        })
    }).await;

    router.register("group/role/set", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { set::handle(body, services, ctx).await })
        })
    }).await;

    tracing::debug!("📋 Role 模块路由注册完成 (transfer_owner, set)");
}

