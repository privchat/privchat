pub mod status;
pub mod subscribe;
pub mod typing;
pub mod unsubscribe;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册在线状态系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // presence/subscribe - 订阅在线状态
    GLOBAL_RPC_ROUTER
        .register(routes::presence::SUBSCRIBE, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { subscribe::handle(params, services, ctx).await })
            }
        })
        .await;

    // presence/unsubscribe - 取消订阅
    GLOBAL_RPC_ROUTER
        .register(routes::presence::UNSUBSCRIBE, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { unsubscribe::handle(params, services, ctx).await })
            }
        })
        .await;

    // presence/typing - 输入状态通知
    GLOBAL_RPC_ROUTER
        .register(routes::presence::TYPING, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { typing::handle(params, services, ctx).await })
            }
        })
        .await;

    // presence/status/get - 批量查询在线状态
    GLOBAL_RPC_ROUTER
        .register(routes::presence::STATUS_GET, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { status::handle(params, services, ctx).await })
            }
        })
        .await;

    tracing::debug!("🔔 Presence 系统路由注册完成 (subscribe, unsubscribe, typing, status)");
}
