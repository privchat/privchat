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

//! 文件上传路由
//!
//! 路由：POST /api/app/files/upload
//! 认证：需要 X-Upload-Token header

use axum::{extract::DefaultBodyLimit, extract::State, routing::post, Router};
use axum_extra::extract::Multipart;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::Serialize;
use tracing::info;

use crate::error::ServerError;
use crate::http::{ApiEnvelope, ApiResult, FileServerState};

/// 文件上传响应（spec SERVICE_RESPONSE_ENVELOPE_SPEC §0：所有 HTTP 接口走统一信封）。
#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub file_id: u64,
    pub file_url: String,
    /// P1 缩略图 URL；当前未生成时为 null。
    pub thumbnail_url: Option<String>,
    pub file_size: u64,
    pub original_size: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub mime_type: String,
    pub uploaded_at: u64,
    pub storage_source_id: u32,
}

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
pub fn create_route() -> Router<FileServerState> {
    Router::new()
        .route("/api/app/files/upload", post(upload_file))
        // Keep this above token max_size (100MB) so multipart parsing won't fail
        // before business validation runs.
        .layer(DefaultBodyLimit::max(120 * 1024 * 1024))
}

/// 流式接收 multipart：file 字段按 chunk 直写存储（大小硬顶即时校验），
/// 其余字段照常收集；收完后做加密结构校验。任何失败都会清理已写入的半文件。
///
/// 返回 (upload, filename, mime_type, business_id, encryption_version, cek)。
async fn receive_streaming(
    state: &FileServerState,
    token_info: &crate::service::upload_token_service::UploadToken,
    multipart: &mut Multipart,
) -> Result<
    (
        crate::service::file_service::StreamingUpload,
        String,
        String,
        Option<String>,
        i32,
        Option<String>,
    ),
    ServerError,
> {
    let mut upload: Option<crate::service::file_service::StreamingUpload> = None;
    let mut filename: Option<String> = None;
    let mut mime_type: Option<String> = None;
    let mut business_id: Option<String> = None;
    // 附件加密 v1：encryption_version 0/1；version=1 时 cek=base64url(32B)，nonce 在 blob 头。
    let mut encryption_version: i32 = 0;
    let mut cek: Option<String> = None;

    // 失败路径统一清理半文件后返回错误。
    macro_rules! fail {
        ($upload:ident, $err:expr) => {{
            if let Some(u) = $upload.take() {
                u.abort().await;
            }
            return Err($err);
        }};
    }

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => fail!(
                upload,
                ServerError::Validation(format!("解析 multipart 失败: {}", e))
            ),
        };
        let field_name = field.name().unwrap_or("").to_string();
        match field_name.as_str() {
            "file" => {
                if upload.is_some() {
                    fail!(
                        upload,
                        ServerError::Validation("重复的 file 字段".to_string())
                    );
                }
                let fname = field
                    .file_name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "file.bin".to_string());
                let mime = field
                    .content_type()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                let mut sink = match state
                    .file_service
                    .begin_streaming_upload(&mime, &fname, token_info.max_size)
                    .await
                {
                    Ok(sink) => sink,
                    Err(e) => fail!(upload, e),
                };
                let mut field = field;
                loop {
                    match field.chunk().await {
                        Ok(Some(chunk)) => {
                            if let Err(e) = sink.write_chunk(chunk).await {
                                sink.abort().await;
                                fail!(upload, e);
                            }
                        }
                        Ok(None) => break,
                        Err(e) => {
                            sink.abort().await;
                            fail!(
                                upload,
                                ServerError::Validation(format!("读取文件数据失败: {}", e))
                            );
                        }
                    }
                }
                filename = Some(fname);
                mime_type = Some(mime);
                upload = Some(sink);
            }
            "business_id" => {
                if let Ok(s) = field.text().await {
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        business_id = Some(s);
                    }
                }
            }
            "encryption_version" => {
                if let Ok(s) = field.text().await {
                    encryption_version = s.trim().parse::<i32>().unwrap_or(0);
                }
            }
            "cek" => {
                if let Ok(s) = field.text().await {
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        cek = Some(s);
                    }
                }
            }
            _ => {}
        }
    }

    let Some(sink) = upload.take() else {
        return Err(ServerError::Validation("缺少文件数据".to_string()));
    };
    let mut upload = Some(sink);
    let filename = filename.unwrap_or_else(|| "file.bin".to_string());
    let mime_type = mime_type.unwrap_or_else(|| "application/octet-stream".to_string());

    // 附件加密结构校验（服务端不解密、不验 GCM tag，仅防脏数据入库；ATTACHMENT_ENCRYPTION_SPEC §3.2）。
    // 注意：cek 绝不进日志。
    match encryption_version {
        0 => {
            if cek.is_some() {
                fail!(
                    upload,
                    ServerError::Validation("encryption_version=0 时不应携带 cek".to_string())
                );
            }
        }
        1 => {
            let Some(cek_str) = cek.as_deref() else {
                fail!(
                    upload,
                    ServerError::Validation("encryption_version=1 缺少 cek".to_string())
                );
            };
            let decoded = match URL_SAFE_NO_PAD.decode(cek_str.as_bytes()) {
                Ok(d) => d,
                Err(_) => fail!(
                    upload,
                    ServerError::Validation("cek 不是合法 base64url".to_string())
                ),
            };
            if decoded.len() != 32 {
                fail!(
                    upload,
                    ServerError::Validation(format!(
                        "cek 解码后必须为 32 字节，实际 {}",
                        decoded.len()
                    ))
                );
            }
            // blob = nonce(12) || ciphertext(>=0) || tag(16)，最少 28 字节
            let written = upload.as_ref().map(|u| u.written()).unwrap_or(0);
            if written < 28 {
                fail!(
                    upload,
                    ServerError::Validation(format!(
                        "加密 blob 至少 28 字节（12 nonce + 16 tag），实际 {}",
                        written
                    ))
                );
            }
        }
        v => {
            fail!(
                upload,
                ServerError::Validation(format!("不支持的 encryption_version: {}", v))
            );
        }
    }

    let sink = upload.take().expect("upload present after validation");
    Ok((
        sink,
        filename,
        mime_type,
        business_id,
        encryption_version,
        cek,
    ))
}

/// 文件上传处理器
async fn upload_file(
    State(state): State<FileServerState>,
    headers: axum::http::HeaderMap,
    mut multipart: Multipart,
) -> ApiResult<UploadResponse> {
    // 提取 X-Upload-Token header
    let upload_token = headers
        .get("X-Upload-Token")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| ServerError::Validation("缺少 X-Upload-Token header".to_string()))?;

    // P0-10：token 不落明文日志，只留前缀定位。
    tracing::info!(
        "🔐 验证上传 token: {}…",
        upload_token.chars().take(8).collect::<String>()
    );

    // 验证 token
    let token_info = state
        .upload_token_service
        .validate_token(upload_token)
        .await?;

    // 标记 token 已使用（Redis 路径 GETDEL 原子消费，跨实例一次性）
    state
        .upload_token_service
        .mark_token_used(upload_token)
        .await?;

    tracing::info!("✅ Token 验证通过，用户: {}", token_info.user_id);

    // P0-10：流式接收——数据边收边写存储，任何失败清理半文件，不再全量进内存。
    let (upload, filename, mime_type, business_id, encryption_version, cek) =
        receive_streaming(&state, &token_info, &mut multipart).await?;

    let uploader_id = token_info.user_id;
    let uploader_ip = client_ip_from_headers(&headers);
    let file_service = &state.file_service;

    info!(
        "📤 上传文件: {} ({} bytes, {}) from 用户 {}, ip: {}",
        filename,
        upload.written(),
        mime_type,
        uploader_id,
        uploader_ip.as_deref().unwrap_or("-")
    );

    // 只做存储；业务类型来自 token，business_id 可选（表单或后续 update_business 关联）
    let metadata = file_service
        .commit_streaming_upload(
            upload,
            filename,
            mime_type,
            uploader_id,
            uploader_ip,
            token_info.business_type.clone(),
            business_id,
            encryption_version,
            cek,
        )
        .await?;

    info!("✅ 文件上传成功: {}", metadata.file_id);

    // 返回响应（含 storage_source_id，便于客户端写入消息 content，未来多存储源）
    Ok(ApiEnvelope::ok(UploadResponse {
        file_id: metadata.file_id,
        file_url: file_service.build_access_url(&metadata.file_path, metadata.storage_source_id),
        thumbnail_url: None,
        file_size: metadata.file_size,
        original_size: metadata.original_size,
        width: metadata.width,
        height: metadata.height,
        mime_type: metadata.mime_type,
        uploaded_at: metadata.uploaded_at,
        storage_source_id: metadata.storage_source_id,
    }))
}
