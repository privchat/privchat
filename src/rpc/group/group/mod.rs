pub mod add;
pub mod create;
pub mod info;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册群组模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("group/group/create", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { create::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/group/info", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { info::handle(body, services, ctx).await })
            })
        })
        .await;

    // group/group/list 已废弃，列表数据由 entity/sync_entities 同步，客户端从本地读 get_groups

    tracing::debug!("📋 Group 模块路由注册完成");
}
