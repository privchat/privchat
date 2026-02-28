// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! 文件 URL 获取路由
//!
//! 路由：GET /api/app/files/{file_id}/url

use axum::{
    extract::{Path, State},
    response::Json,
    routing::get,
    Router,
};
use serde_json::{json, Value};
use tracing::info;

use crate::error::{Result, ServerError};
use crate::http::FileServerState;

/// 创建 URL 路由
pub fn create_route() -> Router<FileServerState> {
    Router::new().route("/api/app/files/{file_id}/url", get(get_file_url))
}

/// 获取文件 URL 处理器
async fn get_file_url(
    State(state): State<FileServerState>,
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

    info!("🔗 获取文件 URL: {} (用户: {})", file_id, user_id);

    let url_response = file_service.get_file_url(file_id, user_id).await?;

    // 返回响应（含 storage_source_id，便于客户端按源区分）
    Ok(Json(json!({
        "file_url": url_response.file_url,
        "thumbnail_url": url_response.thumbnail_url,
        "expires_at": url_response.expires_at,
        "file_size": url_response.file_size,
        "mime_type": url_response.mime_type,
        "storage_source_id": url_response.storage_source_id,
    })))
}
