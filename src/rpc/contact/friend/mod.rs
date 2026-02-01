pub mod apply;
pub mod accept;
pub mod remove;
pub mod pending;
pub mod check;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册好友模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();
    
    // ✨ 使用路由常量代替硬编码字符串
    router.register(routes::friend::APPLY, {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { apply::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register(routes::friend::ACCEPT, {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { accept::handle(body, services, ctx).await })
        })
    }).await;
    
    // contact/friend/list 已废弃，列表数据由 entity/sync_entities 同步，客户端从本地读 get_friends
    // router.register(routes::friend::LIST, ...) 已移除

    router.register(routes::friend::DELETE, {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { remove::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register(routes::friend::PENDING, {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { pending::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register(routes::friend::CHECK, {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { check::handle(body, services, ctx).await })
        })
    }).await;
    
    tracing::debug!("📋 Friend 模块路由注册完成 (使用 privchat-protocol routes)");
} 