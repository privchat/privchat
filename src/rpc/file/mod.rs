//! 文件相关 RPC 接口

pub mod request_upload_token;
pub mod upload_callback;
pub mod validate_token;

pub use request_upload_token::request_upload_token;
pub use upload_callback::upload_callback;
pub use validate_token::validate_upload_token;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册文件系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 客户端 RPC（公开接口）
    let services1 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/request_upload_token", move |params, _ctx| {
            let services = services1.clone();
            Box::pin(async move { request_upload_token(services, params).await })
        })
        .await;

    // 内部 RPC（仅文件服务器调用）
    let services2 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/validate_token", move |params, _ctx| {
            let services = services2.clone();
            Box::pin(async move { validate_upload_token(services, params).await })
        })
        .await;

    let services3 = services.clone();
    GLOBAL_RPC_ROUTER
        .register("file/upload_callback", move |params, _ctx| {
            let services = services3.clone();
            Box::pin(async move { upload_callback(services, params).await })
        })
        .await;

    tracing::debug!("📁 File 系统路由注册完成");
}
