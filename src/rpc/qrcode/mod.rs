pub mod generate;
pub mod list;
pub mod refresh;
pub mod resolve;
pub mod revoke;
pub mod utils;

// 重新导出工具函数
pub use utils::{extract_qr_key_from_url, extract_token_from_url, generate_random_token};

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册 QR 码模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    // 生成 QR 码
    router
        .register("qrcode/generate", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { generate::handle(body, services, ctx).await })
            })
        })
        .await;

    // 解析 QR 码
    router
        .register("qrcode/resolve", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { resolve::handle(body, services, ctx).await })
            })
        })
        .await;

    // 刷新 QR 码
    router
        .register("qrcode/refresh", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { refresh::handle(body, services, ctx).await })
            })
        })
        .await;

    // 撤销 QR 码
    router
        .register("qrcode/revoke", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { revoke::handle(body, services, ctx).await })
            })
        })
        .await;

    // 列出 QR 码
    router
        .register("qrcode/list", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 QRCode 模块路由注册完成 (generate, resolve, refresh, revoke, list)");
}
