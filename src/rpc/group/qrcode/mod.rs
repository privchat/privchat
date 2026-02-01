pub mod generate;
pub mod join;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册二维码模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router.register("group/qrcode/generate", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { generate::handle(body, services, ctx).await })
        })
    }).await;

    router.register("group/join/qrcode", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { join::handle(body, services, ctx).await })
        })
    }).await;

    tracing::debug!("📋 QRCode 模块路由注册完成 (generate, join)");
}

