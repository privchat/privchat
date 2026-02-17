pub mod auth;
pub mod privacy;
pub mod profile;
pub mod search;
pub mod settings;
pub mod user;

use super::RpcServiceContext;

/// 注册账户系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    user::register_routes(services.clone()).await;
    auth::register_routes(services.clone()).await; // 测试用的认证接口
    search::register_routes(services.clone()).await; // 用户搜索接口
    privacy::register_routes(services.clone()).await; // 隐私设置接口
    settings::register_routes(services.clone()).await; // 用户设置（ENTITY_SYNC_V1 user_settings）
                                                       // TODO: 暂时注释 profile 模块
                                                       // profile::register_routes(services.clone()).await;

    tracing::debug!("📋 Account 系统路由注册完成 (user, auth, search, privacy, settings 模块)");
}
