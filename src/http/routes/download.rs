//! 文件下载路由
//! 
//! 路由：GET /api/app/files/{file_id}

use axum::{
    extract::{State, Path},
    response::IntoResponse,
    routing::get,
    Router,
    http::{StatusCode, header},
};
use tracing::info;

use crate::error::{Result, ServerError};
use crate::http::HttpServerState;

/// 创建下载路由
pub fn create_route() -> Router<HttpServerState> {
    Router::new().route("/api/app/files/{file_id}", get(download_file))
}

/// 文件下载处理器
async fn download_file(
    State(state): State<HttpServerState>,
    Path(file_id): Path<String>,
) -> Result<impl IntoResponse> {
    let file_id = file_id.parse::<u64>()
        .map_err(|_| ServerError::Validation("file_id 必须为数字".to_string()))?;
    let file_service = &state.file_service;
    info!("📥 下载文件: {}", file_id);

    let metadata = file_service.get_file_metadata(file_id).await?
        .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;

    let file_data = file_service.read_file(file_id).await?;

    // 构建响应头
    let headers = [
        (header::CONTENT_TYPE, metadata.mime_type),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", metadata.original_filename),
        ),
        (header::CONTENT_LENGTH, file_data.len().to_string()),
    ];

    Ok((StatusCode::OK, headers, file_data))
}

