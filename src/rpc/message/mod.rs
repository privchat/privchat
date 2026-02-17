pub mod history;
pub mod reaction;
pub mod revoke;
pub mod status;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册消息系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    history::register_routes(services.clone()).await;
    status::register_routes(services.clone()).await;
    reaction::register_routes(services.clone()).await;

    // 注册消息撤回路由
    GLOBAL_RPC_ROUTER
        .register(routes::message::REVOKE, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { revoke::handle(params, services, ctx).await })
            }
        })
        .await;

    tracing::debug!("📋 Message 系统路由注册完成 (history, status, reaction, revoke)");
}
