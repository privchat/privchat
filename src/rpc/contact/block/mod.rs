pub mod add;
pub mod list;

use super::super::RpcServiceContext;

/// 注册 block 模块的所有路由
pub async fn register_routes(_services: RpcServiceContext) {
    // TODO: 暂时不实现block功能
    tracing::debug!("📋 block 模块路由注册完成 (暂未实现)");
}
