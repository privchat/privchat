pub mod create;
pub mod subscribe;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册 channel 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();
    
    router.register("channel/channel/create", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { create::handle(body, services, ctx).await })
        })
    }).await;
    
    router.register("channel/channel/subscribe", {
        let services = services.clone();
        Box::new(move |body, ctx| {
            let services = services.clone();
            Box::pin(async move { subscribe::handle(body, services, ctx).await })
        })
    }).await;

    // channel/channel/list 已废弃，列表数据由 entity/sync_entities 同步，客户端从本地读 get_channels / get_channel_list_entries

    tracing::debug!("📋 channel 模块路由注册完成");
}
