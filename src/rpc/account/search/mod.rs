pub mod by_qrcode;
pub mod query;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 search 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let services_qrcode = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_search::BY_QRCODE, move |body, ctx| {
            let services = services_qrcode.clone();
            Box::pin(async move { by_qrcode::handle(body, services, ctx).await })
        })
        .await;

    let services_query = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_search::QUERY, move |body, ctx| {
            let services = services_query.clone();
            Box::pin(async move { query::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("🔧 Search 模块路由注册完成");
}
