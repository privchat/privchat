//! RPC: 上传完成回调（内部 RPC）
//!
//! 由文件服务器调用，通知业务服务器文件上传完成

use serde_json::{json, Value};
use tracing::warn;

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};

/// 上传完成回调
pub async fn upload_callback(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // 解析参数
    let upload_token = params["upload_token"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 upload_token 参数".to_string()))?;

    let file_id = params["file_id"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_id 参数".to_string()))?
        .to_string();

    let _file_url = params["file_url"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_url 参数".to_string()))?
        .to_string();

    let _thumbnail_url = params["thumbnail_url"].as_str().map(|s| s.to_string());

    let file_size = params["file_size"]
        .as_u64()
        .ok_or_else(|| RpcError::validation("缺少 file_size 参数".to_string()))?;

    let _original_size = params["original_size"].as_u64();

    let _mime_type = params["mime_type"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 mime_type 参数".to_string()))?
        .to_string();

    let width = params["width"].as_u64().map(|v| v as u32);
    let height = params["height"].as_u64().map(|v| v as u32);

    tracing::debug!(
        "📤 文件上传完成回调: file_id={}, size={} bytes",
        file_id,
        file_size
    );

    // 验证 token 是否存在（已使用）
    match services
        .upload_token_service
        .validate_token(upload_token)
        .await
    {
        Ok(_) => {
            // Token 仍然有效，这不应该发生（应该在验证时已标记为已使用）
            warn!("⚠️ 上传回调时 token 仍然有效: {}", upload_token);
        }
        Err(_) => {
            // Token 已失效（已使用或过期），这是预期行为
        }
    }

    // TODO: 记录文件元数据到数据库
    // TODO: 更新用户配额
    // TODO: 触发后续业务逻辑（如媒体处理、内容审核）

    // 清理 token
    let _ = services
        .upload_token_service
        .remove_token(upload_token)
        .await;

    Ok(json!({
        "success": true,
        "message": "文件上传成功",
    }))
}
