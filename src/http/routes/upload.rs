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
        // 从业务硬顶推导，不再写死一个会跟业务限额分家的数字：body limit 必须高于最大
        // 硬顶，否则 multipart 会在业务校验跑起来之前被拒，用户拿到一个没有业务含义的 413。
        .layer(DefaultBodyLimit::max(
            crate::model::file_upload::FileType::http_body_limit_bytes(),
        ))
}

/// 流式接收 multipart：file 字段按 chunk 直写存储（大小硬顶即时校验），
/// 其余字段照常收集；收完后做加密结构校验。任何失败都会清理已写入的半文件。
///
/// 返回 (upload, filename, mime_type, business_id, encryption_version, cek, transform_version)。
async fn receive_streaming(
    state: &FileServerState,
    token_info: &crate::service::upload_token_service::ValidatedUploadToken,
    reserved_file_id: Option<u64>,
    multipart: &mut Multipart,
) -> Result<
    (
        crate::service::file_service::StreamingUpload,
        String,
        String,
        Option<String>,
        i32,
        Option<String>,
        i32,
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
    // 客户端处理版本：参与秒传身份。压缩算法一变就是另一份字节，不能命中旧对象。
    let mut transform_version: i32 = 0;

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
                // 🔴 MIME 以 **token** 为准，multipart 只作老协议兜底。
                //
                // 加密上传的 body 是不透明字节，客户端普遍标 application/octet-stream；
                // 按它入库会把 image/jpeg、video/mp4 丢成 octet-stream，
                // 下载响应的 Content-Type 也跟着错。
                let mime = token_info
                    .mime_type
                    .clone()
                    .filter(|m| !m.trim().is_empty())
                    .or_else(|| field.content_type().map(|s| s.to_string()))
                    .unwrap_or_else(|| "application/octet-stream".to_string());
                let mut sink = match state
                    .file_service
                    .begin_streaming_upload(
                        &mime,
                        &fname,
                        token_info.max_size,
                        reserved_file_id,
                        Some(token_info.file_type.clone()),
                        token_info.user_id,
                        &token_info.upload_id,
                    )
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
            "transform_version" => {
                if let Ok(s) = field.text().await {
                    transform_version = s.trim().parse::<i32>().unwrap_or(0);
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
        transform_version,
    ))
}

/// 已完成的上传：按 `file_id` 回读、**核对身份**并构造与首次一致的响应。
///
/// 幂等重试拿到的必须是**同一份结果**，所以这里不重新计算任何东西，只回读。
async fn completed_response(
    state: &FileServerState,
    token: &crate::service::upload_token_service::ValidatedUploadToken,
    file_id: u64,
) -> ApiResult<UploadResponse> {
    let meta = state
        .file_service
        .get_file_metadata(file_id)
        .await?
        .ok_or_else(|| {
            ServerError::Internal(format!("会话记录指向的 file_id={file_id} 读不到"))
        })?;
    if !token.matches_file(&meta) {
        return Err(ServerError::Internal(format!(
            "file_id={file_id} 与本次上传的身份不符，拒绝返回"
        )));
    }
    tracing::info!("♻️ 重复上传请求，返回原 file_id={file_id}");
    Ok(ApiEnvelope::ok(UploadResponse {
        file_id: meta.file_id,
        file_url: state
            .file_service
            .build_access_url(&meta.file_path, meta.storage_source_id),
        thumbnail_url: None,
        file_size: meta.file_size,
        original_size: meta.original_size,
        width: meta.width,
        height: meta.height,
        mime_type: meta.mime_type,
        uploaded_at: meta.uploaded_at,
        storage_source_id: meta.storage_source_id,
    }))
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

    // 🔴 统一验证入口：签名 token 与旧 UUID 各走各的，输出同一个模型。
    // 三段点分的串**只按签名验**，失败即拒，绝不回退 Redis（那是降级通道）。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let token_info = state
        .upload_token_service
        .validate_any(now_secs, upload_token)
        .await?;

    // 🔴 预检命中时签发的是 claim 用途的 token，不能拿来传字节。
    if token_info.purpose
        != crate::service::upload_token_service::UploadTokenPurpose::Upload
    {
        return Err(ServerError::Validation(
            "该 token 用于秒传取用，不能用于实体上传".to_string(),
        ));
    }

    // 🔴 **`GETDEL` 一次性消费已移除。**
    //
    // 它原本同时兼任两件事：防重放，以及串行化并发的整包 POST。两件都由**会话**接管，
    // 业务库不参与：
    //   · 模式锁（`state.mode` / `status`）——同一 upload_id 只允许一条路径，
    //     且整包接收期间独占；
    //   · `reserved_file_id` + 墓碑——重复 POST 复用同一个预留 id，落库时撞主键即回读。
    // （早期版本曾用 `upload_completion_key` 列做这件事，属把临时态写进业务库，已撤销。）
    let session = crate::service::upload_session::UploadSession::open(
        &state.file_service.upload_session_root()?,
        token_info.user_id,
        &token_info.upload_id,
    )?;

    // 🔴 **幂等出口排在接收 body 之前，真源是会话自己的墓碑。**
    //
    // 取消一次性消费后，重复 POST 是正常现象（响应丢了、客户端重试）。这时正确行为是
    // **立刻返回原来那个 `file_id`**——不是让用户把整个文件再传一遍，更不是因为
    // 「这次上传已完成」而报错（客户端无法把它与失败区分开）。
    //
    // 📌 判据只看**临时会话状态**：上传中间态不进业务库。会话没了就是没了，
    // 客户端重新申请 token 从头传（这正是 `SessionGone` 的语义）。
    if let Some(existing) = session.completed_file_id()? {
        return completed_response(&state, &token_info, existing).await;
    }

    let _mode_guard = session.begin_whole()?;

    // 🔴 **预留必须在收字节之前**，而且要先落盘。
    //
    // 预留写在接收之后的话，传输中途崩溃就没有预留——重试会分配新 id，上一次的
    // 半成品对象没人认领，变成垃圾。
    let reserved = match session.reserved_file_id()? {
        Some(id) => {
            // 🔴 **带着预留 id 回来时，先问正式文件表：这个 id 是不是已经落库了。**
            //
            // 不问的话，下面会用同一个 file_id 推出**同一个正式对象路径**并直接打开
            // writer——那就是在覆盖一个已被提交记录引用的文件；这次再失败，`abort()`
            // 还会把它删掉。这是数据丢失，不是幂等。
            if let Some(meta) = state.file_service.get_file_metadata(id).await? {
                // 🔴 只比 uploader 不够：同一个用户名下有成千上万个附件。
                // 与墓碑返回、主键冲突分支共用 `matches_file`（§身份判据只有一处）。
                if !token_info.matches_file(&meta) {
                    return Err(ServerError::Internal(format!(
                        "预留的 file_id={id} 与本次上传身份不符，拒绝继续"
                    )));
                }
                tracing::info!("♻️ 预留的 file_id={id} 已落库，补写墓碑并返回");
                let _ = session.mark_completed(id);
                return completed_response(&state, &token_info, id).await;
            }
            Some(id)
        }
        None => {
            // 先分配、先落盘，再开 writer。
            let id = state.file_service.reserve_file_id().await?;
            session.reserve_file_id(id)?;
            Some(id)
        }
    };

    tracing::info!(
        "✅ Token 验证通过，用户: {} upload_id: {} 预留 file_id: {:?}",
        token_info.user_id,
        token_info.upload_id,
        reserved
    );

    // P0-10：流式接收——数据边收边写存储，任何失败清理半文件，不再全量进内存。
    let (upload, filename, mime_type, business_id, encryption_version, cek, transform_version) =
        receive_streaming(&state, &token_info, reserved, &mut multipart).await?;

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
            // 处理版本只是元数据，不参与秒传身份（身份只看内容摘要）。
            transform_version,
            // 🔴 内容摘要取自 **token**，不取表单。表单里的值是这一次请求带来的，
            // 客户端可以在 prepare 之后换掉；token 里那份是 prepare 当时签下的。
            token_info.sha256.clone(),
            token_info.sealed_blob_size,
        )
        .await?;

    // 窗口三：记录已经提交，墓碑还没写。
    crate::service::file_service::crash_point("after_commit_before_tombstone");

    // 成功：把会话推到 Completed（墓碑），迟到的重复请求由它与幂等键一起回答。
    // 失败路径不走这里——guard 的 Drop 会把状态放回 Idle，让同一张 token 能重试。
    if let Err(e) = _mode_guard.complete(metadata.file_id) {
        // 落库已经成功，会话状态没写上只影响墓碑；下次请求会走幂等出口拿回同一个
        // file_id，所以不把整个上传判失败。
        tracing::warn!("写入上传会话完成状态失败 file_id={}: {e}", metadata.file_id);
    }

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
