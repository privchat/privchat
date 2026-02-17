//! 获取表情包库详情 RPC 接口

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理获取表情包库详情请求
///
/// 请求参数：
/// ```json
/// {
///   "package_id": "classic"
/// }
/// ```
///
/// 返回格式：
/// ```json
/// {
///   "package": {
///     "package_id": "classic",
///     "name": "经典表情",
///     "thumbnail_url": "...",
///     "author": "PrivChat",
///     "description": "...",
///     "sticker_count": 10,
///     "stickers": [
///       {
///         "sticker_id": "classic_001",
///         "package_id": "classic",
///         "image_url": "...",
///         "alt_text": "微笑",
///         "emoji": "😊",
///         "width": 128,
///         "height": 128,
///         "mime_type": "image/png"
///       }
///     ]
///   }
/// }
/// ```
pub async fn handle(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    tracing::debug!("🔧 处理表情包库详情请求: {:?}", params);

    // 提取 package_id
    let package_id = params["package_id"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 package_id 参数".to_string()))?;

    // 获取表情包库详情
    let package = services
        .sticker_service
        .get_package_detail(package_id)
        .await
        .ok_or_else(|| RpcError::not_found(format!("表情包库 {} 不存在", package_id)))?;

    tracing::debug!("✅ 返回表情包库详情: {}", package_id);

    Ok(json!({
        "package": package
    }))
}
