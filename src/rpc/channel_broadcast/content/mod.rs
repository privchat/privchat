pub mod publish;
pub mod list;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册 content 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();
    
    router.register("channel/content/publish", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { publish::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register("channel/content/list", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { list::handle(body, services, ctx).await })
        })
    }).await;
    
    tracing::debug!("📋 content 模块路由注册完成");
}
