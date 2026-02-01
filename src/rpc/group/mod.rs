pub mod group;
pub mod member;
pub mod qrcode;
pub mod settings;
pub mod role;
pub mod approval;

use super::RpcServiceContext;

/// 注册群组系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    group::register_routes(services.clone()).await;
    member::register_routes(services.clone()).await;
    qrcode::register_routes(services.clone()).await;
    settings::register_routes(services.clone()).await;
    role::register_routes(services.clone()).await;
    approval::register_routes(services.clone()).await;
    
    tracing::info!("📋 Group 系统路由注册完成 (group, member, qrcode, settings, role, approval)");
} 