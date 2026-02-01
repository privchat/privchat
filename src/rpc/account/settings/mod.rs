//! 用户设置模块（ENTITY_SYNC_V1 user_settings）
//!
//! - account/settings/update：单条或批量更新，供多端同步

pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册用户设置模块路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("account/settings/update", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { update::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Account settings 模块路由注册完成 (update)");
}
