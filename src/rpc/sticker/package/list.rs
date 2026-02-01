//! 获取表情包库列表 RPC 接口

use serde_json::{json, Value};
use crate::rpc::error::RpcResult;
use crate::rpc::RpcServiceContext;

/// 处理获取表情包库列表请求
/// 
/// 请求参数：（无）
/// 
/// 返回格式：
/// ```json
/// {
///   "packages": [
///     {
///       "package_id": "classic",
///       "name": "经典表情",
///       "thumbnail_url": "...",
///       "author": "PrivChat",
///       "description": "...",
///       "sticker_count": 10
///     }
///   ]
/// }
/// ```
pub async fn handle(services: RpcServiceContext, _params: Value) -> RpcResult<Value> {
    tracing::info!("🔧 处理表情包库列表请求");
    
    // 获取所有表情包库
    let packages = services.sticker_service.list_packages().await;
    
    tracing::info!("✅ 返回 {} 个表情包库", packages.len());
    
    Ok(json!({
        "packages": packages
    }))
}

