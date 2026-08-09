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

//! RPC: 请求上传许可

use serde_json::Value;
use tracing::warn;

use crate::rpc::{RpcContext, RpcError, RpcResult, RpcServiceContext};
use crate::service::FileType;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::{FileRequestUploadTokenRequest, FileRequestUploadTokenResponse};

/// 请求上传 token
pub async fn request_upload_token(
    services: RpcServiceContext,
    params: Value,
    ctx: RpcContext,
) -> RpcResult<Value> {
    // ✨ 使用协议层类型自动反序列化
    let request: FileRequestUploadTokenRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // uploader 必须取自鉴权 session，绝不信客户端传的 user_id（TS SDK 传 0，且客户端
    // 伪造 uploader 会让 send 校验 file 所有权失效）。这也是 web 发文件失败的根因。
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
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
        // 用协议里的 FileTooLarge，别再混进 InvalidParams：这是客户端唯一能在
        // 「传之前」拿到的超限信号，必须可被识别成一句具体的提示，而不是通用的
        // 「发送失败」。参数不合法和文件太大对用户是两件完全不同的事。
        return Err(RpcError::from_code(
            ErrorCode::FileTooLarge,
            format!("文件大小超过限制（最大 {} MB）", max_size / 1024 / 1024),
        ));
    }

    // 秒传：客户端报了最终内容的摘要，而服务端已经有这份字节 → 不必再传一遍。
    //
    // 「转发」在本产品里就走这条路：转发的人本来就下载过那张图，他重新发一条
    // 同样的消息，附件在这里命中，于是既没有转发协议，也没有第二条上传路径。
    if let Some(sha256) = request.sha256.as_deref() {
        let identity = crate::service::media_blob_service::BlobIdentity::parse(
            sha256,
            request.transform_version,
        )
        .map_err(|e| RpcError::validation(e.to_string()))?;

        let pool = services.channel_service.pool();
        if let Some(blob) = crate::service::media_blob_service::find_blob(pool, &identity)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
        {
            // 🔴 命中不等于放行。只凭摘要就发句柄 = 「知道 hash 就等于拥有文件」，
            // 判据是他**已经有权读到这份内容**（自己传过，或能读到引用它的消息）。
            let may_reuse = crate::service::media_blob_service::may_reuse(
                pool,
                &services.message_repository,
                &services.channel_service,
                blob.blob_id,
                user_id,
            )
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?;

            if may_reuse {
                // 建的是**当前用户自己的**新句柄。后续发消息时的归属校验
                // （uploader_id = sender_id）因此自然通过，不需要任何专用分支。
                let file_id = services
                    .file_service
                    .create_handle_for_blob(
                        &blob,
                        user_id,
                        filename.as_deref().unwrap_or("file.bin"),
                        file_type.as_str(),
                        &business_type,
                    )
                    .await
                    .map_err(|e| RpcError::internal(e.to_string()))?;

                tracing::info!(
                    "⚡ 秒传命中: user={} blob={} → file_id={}",
                    user_id,
                    blob.blob_id,
                    file_id
                );
                let response = FileRequestUploadTokenResponse {
                    token: String::new(),
                    upload_url: String::new(),
                    already_exists: true,
                    file_id: file_id.to_string(),
                    expires_at: None,
                    max_size: None,
                };
                return serde_json::to_value(response)
                    .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)));
            }
        }
    }

    // 生成上传 token（将 u64 转换为 String）
    let token = services
        .upload_token_service
        .generate_token(user_id, file_type, max_size, business_type, filename)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    // 构建上传 URL（必须使用显式配置，禁止默认值回退）
    let upload_url = services
        .config
        .file_api_base_url
        .as_ref()
        .filter(|base_url| !base_url.trim().is_empty())
        .map(|base_url| format!("{}/files/upload", base_url.trim_end_matches('/')))
        .ok_or_else(|| {
            RpcError::internal("缺少配置: file_api_base_url，拒绝签发上传 token".to_string())
        })?;

    let response = FileRequestUploadTokenResponse {
        token: token.token.clone(),
        upload_url,
        already_exists: false,
        // request_upload_token 阶段尚未落盘具体文件，保留兼容空值
        file_id: String::new(),
        expires_at: Some(token.expires_at.timestamp()),
        max_size: Some(token.max_size),
    };

    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// 根据文件类型获取最大文件大小限制。
///
/// 唯一来源是 [`FileType::max_size_bytes`]，这里不再维护第二张表——签发时说 200MB、
/// 写入时按 100MB 掐断，等于让用户白传几分钟才失败。
fn get_max_size_for_type(file_type: &FileType) -> i64 {
    file_type.max_size_bytes() as i64
}

/// 让「签发限额 == 写入硬顶」这条不变量可被测试直接盯住，而不是靠人肉对两张表。
#[cfg(test)]
pub(crate) fn max_size_for_type_for_tests(file_type: &FileType) -> i64 {
    get_max_size_for_type(file_type)
}
