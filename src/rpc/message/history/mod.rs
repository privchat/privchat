pub mod get;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册 history 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();
    
    router.register("message/history/get", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { get::handle(body, services, ctx).await })
        })
    }).await;
    
    tracing::debug!("📋 history 模块路由注册完成（message/history/search 已移除，搜索由客户端本地实现）");
}
