//! RPC: 请求上传许可

use serde_json::{json, Value};
use tracing::warn;

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};
use crate::service::FileType;
use privchat_protocol::rpc::FileRequestUploadTokenRequest;

/// 请求上传 token
pub async fn request_upload_token(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // ✨ 使用协议层类型自动反序列化
    let request: FileRequestUploadTokenRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = request.user_id;
    let file_type_str = &request.file_type;
    let file_size = request.file_size;
    let mime_type = request.mime_type;
    let business_type = request.business_type;
    let filename = request.filename;

    let file_type = FileType::from_str(file_type_str)
        .ok_or_else(|| RpcError::validation(format!("无效的文件类型: {}", file_type_str)))?;

    tracing::debug!(
        "📥 用户 {} 请求上传许可: 类型={}, 大小={} bytes, 业务={}",
        user_id,
        file_type_str,
        file_size,
        business_type
    );

    // 业务检查
    // TODO: 检查用户权限、存储配额、频率限制等

    // 检查文件大小限制
    let max_size = get_max_size_for_type(&file_type);
    if file_size > max_size {
        warn!("❌ 文件大小超限: {} bytes > {} bytes", file_size, max_size);
        return Err(RpcError::validation(format!(
            "文件大小超过限制（最大 {} MB）",
            max_size / 1024 / 1024
        )));
    }

    // 生成上传 token（将 u64 转换为 String）
    let token = services
        .upload_token_service
        .generate_token(user_id, file_type, max_size, business_type, filename)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    // 构建上传 URL（使用配置的文件服务 API 基础 URL）
    let upload_url = services
        .config
        .file_api_base_url
        .as_ref()
        .map(|base_url| format!("{}/files/upload", base_url.trim_end_matches('/')))
        .unwrap_or_else(|| {
            // 如果没有配置，使用默认值（仅用于开发环境）
            format!(
                "http://localhost:{}/api/app/files/upload",
                services.config.http_file_server_port
            )
        });

    Ok(json!({
        "upload_token": token.token,
        "upload_url": upload_url,
        "expires_at": token.expires_at.timestamp(),
        "max_size": token.max_size,
    }))
}

/// 根据文件类型获取最大文件大小限制
fn get_max_size_for_type(file_type: &FileType) -> i64 {
    match file_type {
        FileType::Image => 10 * 1024 * 1024,  // 10 MB
        FileType::Video => 200 * 1024 * 1024, // 200 MB
        FileType::Audio => 50 * 1024 * 1024,  // 50 MB
        FileType::File => 100 * 1024 * 1024,  // 100 MB
        FileType::Other => 50 * 1024 * 1024,  // 50 MB
    }
}
