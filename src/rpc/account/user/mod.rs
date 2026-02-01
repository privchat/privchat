pub mod register;
pub mod detail;
pub mod update;
pub mod share_card;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册用户模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 为每个处理器创建闭包，捕获服务上下文
    let services_register = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::account_user::REGISTER, move |body, ctx| {
        let services = services_register.clone();
        async move { register::handle(body, services, ctx).await }
    }).await;
    
    let services_detail = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::account_user::DETAIL, move |body, ctx| {
        let services = services_detail.clone();
        Box::pin(async move { detail::handle(body, services, ctx).await })
    }).await;
    
    let services_update = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::account_user::UPDATE, move |body, ctx| {
        let services = services_update.clone();
        async move { update::handle(body, services, ctx).await }
    }).await;
    
    let services_share_card = services.clone();
    GLOBAL_RPC_ROUTER.register(routes::account_user::SHARE_CARD, move |body, ctx| {
        let services = services_share_card.clone();
        Box::pin(async move { share_card::handle(body, services, ctx).await })
    }).await;
    
    tracing::debug!("🔧 User 模块路由注册完成");
}