//! 文件删除路由
//!
//! 路由：DELETE /api/app/files/{file_id}

use axum::{
    extract::{Path, State},
    response::Json,
    routing::delete,
    Router,
};
use serde_json::{json, Value};
use tracing::info;

use crate::error::{Result, ServerError};
use crate::http::HttpServerState;

/// 创建删除路由
pub fn create_route() -> Router<HttpServerState> {
    Router::new().route("/api/app/files/{file_id}", delete(delete_file))
}

/// 删除文件处理器
async fn delete_file(
    State(state): State<HttpServerState>,
    Path(file_id): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Value>> {
    let file_id = file_id
        .parse::<u64>()
        .map_err(|_| ServerError::Validation("file_id 必须为数字".to_string()))?;
    let file_service = &state.file_service;
    let user_id_str = params
        .get("user_id")
        .ok_or_else(|| ServerError::Validation("缺少 user_id 参数".to_string()))?;
    let user_id = user_id_str
        .parse::<u64>()
        .map_err(|_| ServerError::Validation(format!("无效的 user_id: {}", user_id_str)))?;

    info!("🗑️ 删除文件: {} (用户: {})", file_id, user_id);

    file_service.delete_file(file_id, user_id).await?;

    Ok(Json(json!({
        "success": true,
        "file_id": file_id,
    })))
}
