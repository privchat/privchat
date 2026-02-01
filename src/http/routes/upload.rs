//! 文件上传路由
//! 
//! 路由：POST /api/app/files/upload
//! 认证：需要 X-Upload-Token header

use axum::{
    extract::State,
    response::Json,
    routing::post,
    Router,
};
use axum_extra::extract::Multipart;
use serde_json::{json, Value};
use tracing::info;

use crate::error::{Result, ServerError};
use crate::http::HttpServerState;

/// 从请求头提取客户端 IP（兼容反向代理：X-Forwarded-For 取第一个，否则 X-Real-IP）
fn client_ip_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("X-Forwarded-For") {
        if let Ok(s) = v.to_str() {
            let ip = s.split(',').next().map(|s| s.trim());
            if let Some(ip) = ip {
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    if let Some(v) = headers.get("X-Real-IP") {
        if let Ok(s) = v.to_str() {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// 创建上传路由
pub fn create_route() -> Router<HttpServerState> {
    Router::new().route("/api/app/files/upload", post(upload_file))
}

/// 文件上传处理器
async fn upload_file(
    State(state): State<HttpServerState>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> Result<Json<Value>> {
    // 提取 X-Upload-Token header
    let upload_token = headers
        .get("X-Upload-Token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ServerError::Validation("缺少 X-Upload-Token header".to_string()))?;
    
    tracing::info!("🔐 验证上传 token: {}", upload_token);
    
    // 验证 token
    let token_info = state.upload_token_service.validate_token(upload_token).await?;
    
    // 标记 token 已使用
    state.upload_token_service.mark_token_used(upload_token).await?;
    
    tracing::info!("✅ Token 验证通过，用户: {}", token_info.user_id);
    
    let file_service = &state.file_service;

    // 上传服务只负责存储：不解析 compress/generate_thumbnail 等处理参数，客户端已在 token 请求时确定类型与限制
    let mut file_data: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut business_id: Option<String> = None;

    // 解析 multipart/form-data：file 必填；business_id 可选（字符串，兼容各类业务）
    while let Some(field) = multipart.next_field().await
        .map_err(|e| ServerError::Validation(format!("解析 multipart 失败: {}", e)))? {
        let field_name = field.name().unwrap_or("");
        match field_name {
            "file" => {
                filename = field.file_name().map(|s| s.to_string());
                mime_type = field.content_type().map(|s| s.to_string());
                let data = field.bytes().await
                    .map_err(|e| crate::error::ServerError::Validation(format!("读取文件数据失败: {}", e)))?;
                file_data = Some(data.to_vec());
            }
            "business_id" => {
                if let Ok(s) = field.text().await {
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        business_id = Some(s);
                    }
                }
            }
            _ => {}
        }
    }

    // 验证必需字段
    let file_data = file_data.ok_or_else(|| ServerError::Validation("缺少文件数据".to_string()))?;
    let filename = filename.ok_or_else(|| ServerError::Validation("缺少文件名".to_string()))?;
    let mime_type = mime_type.ok_or_else(|| ServerError::Validation("缺少 MIME 类型".to_string()))?;
    
    // 从 token_info 获取 uploader_id
    let uploader_id = token_info.user_id;
    
    // 仅做 token 内约定的校验：文件大小不超过 token 签发时的 max_size
    if file_data.len() as i64 > token_info.max_size {
        return Err(ServerError::Validation(format!(
            "文件大小 {} 超过限制 {} bytes",
            file_data.len(),
            token_info.max_size
        )));
    }

    // 从请求头取客户端 IP（兼容反向代理：X-Forwarded-For 取第一个，否则 X-Real-IP）
    let uploader_ip = client_ip_from_headers(&headers);

    info!("📤 上传文件: {} ({} bytes, {}) from 用户 {}, ip: {}", filename, file_data.len(), mime_type, uploader_id, uploader_ip.as_deref().unwrap_or("-"));

    // 只做存储；业务类型来自 token，business_id 可选（表单或后续 update_business 关联）
    let metadata = file_service.upload_file(
        file_data,
        filename,
        mime_type,
        uploader_id,
        uploader_ip,
        token_info.business_type.clone(),
        business_id,
    ).await?;

    info!("✅ 文件上传成功: {}", metadata.file_id);

    // 返回响应（含 storage_source_id，便于客户端写入消息 content，未来多存储源）
    Ok(Json(json!({
        "file_id": metadata.file_id,
        "file_url": file_service.build_access_url(&metadata.file_path, metadata.storage_source_id),
        "thumbnail_url": serde_json::Value::Null,
        "file_size": metadata.file_size,
        "original_size": metadata.original_size,
        "width": metadata.width,
        "height": metadata.height,
        "mime_type": metadata.mime_type,
        "uploaded_at": metadata.uploaded_at,
        "storage_source_id": metadata.storage_source_id,
    })))
}
