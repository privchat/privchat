pub mod read;
pub mod count;
pub mod read_list;
pub mod read_stats;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册 status 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();
    
    router.register("message/status/read", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { read::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register("message/status/count", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { count::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register("message/status/read_list", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { read_list::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register("message/status/read_stats", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { read_stats::handle(body, services, ctx).await })
        })
    }).await;
    
    tracing::debug!("📋 status 模块路由注册完成 (read, count, read_list, read_stats)");
}
