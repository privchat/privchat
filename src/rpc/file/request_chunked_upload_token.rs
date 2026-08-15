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

//! RPC: 申请**分片**上传 token（RESUMABLE_UPLOAD_SPEC §2）。
//!
//! 与 `file/request_upload_token` 是两个接口：调了这条就是要分片，调不通直接报错，
//! 不会静默退化成整包。

use serde_json::Value;

use crate::rpc::{RpcContext, RpcError, RpcResult, RpcServiceContext};
use crate::service::chunked_upload::{ChunkedSession, NewSession, BASE_UNIT};
use crate::service::FileType;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::{
    FileRequestChunkedUploadTokenRequest, FileRequestChunkedUploadTokenResponse,
};

pub async fn request_chunked_upload_token(
    services: RpcServiceContext,
    params: Value,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let request: FileRequestChunkedUploadTokenRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let file_type = FileType::from_str(&request.file_type)
        .ok_or_else(|| RpcError::validation(format!("无效的文件类型: {}", request.file_type)))?;
    if request.file_size <= 0 {
        return Err(RpcError::validation("file_size 必须大于 0".to_string()));
    }
    let max_size = file_type.max_size_bytes() as i64;
    if request.file_size > max_size {
        return Err(RpcError::from_code(
            ErrorCode::FileTooLarge,
            format!("文件大小超过限制（最大 {} MB）", max_size / 1024 / 1024),
        ));
    }
    let sha256 = request.file_hash.trim().to_ascii_lowercase();
    if sha256.len() != 64 || !sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::validation(
            "file_hash 必须是 64 位十六进制（SHA-256）".to_string(),
        ));
    }
    if request.business_type.trim().is_empty() {
        return Err(RpcError::validation("business_type 不能为空".to_string()));
    }
    if let Some(name) = request.filename.as_ref() {
        if name.len() > crate::security::upload_token::MAX_FILENAME_BYTES {
            return Err(RpcError::validation(format!(
                "文件名过长（最多 {} 字节）",
                crate::security::upload_token::MAX_FILENAME_BYTES
            )));
        }
    }

    // ---- 2.1 秒传预检：命中就回 claim_token，不建任何目录 ----
    if !request.force_upload {
        let hit = services
            .file_service
            .find_by_content(&sha256)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
            .is_some();
        if hit {
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // 复用整包那条路径的秒传签发逻辑：一张 purpose=ClaimExisting 的 token。
            let (claim_token, _, _) = services
                .upload_token_service
                .issue(
                    now_secs,
                    user_id,
                    file_type,
                    max_size,
                    request.business_type.clone(),
                    request.filename.clone(),
                    crate::service::upload_token_service::UploadIdentity {
                        sha256: Some(sha256.clone()),
                        declared_size: Some(request.file_size),
                        mime_type: Some(request.mime_type.clone()),
                        transform_version: request.transform_version,
                    },
                    crate::service::upload_token_service::UploadTokenPurpose::ClaimExisting,
                    None,
                )
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?;
            let response = FileRequestChunkedUploadTokenResponse {
                already_exists: true,
                claim_token: Some(claim_token),
                ..Default::default()
            };
            return serde_json::to_value(response)
                .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)));
        }
    }

    // ---- 2.2 建会话 ----
    let upload_url = services
        .config
        .file_api_base_url
        .as_ref()
        .filter(|base_url| !base_url.trim().is_empty())
        .map(|base_url| format!("{}/files", base_url.trim_end_matches('/')))
        .ok_or_else(|| {
            RpcError::internal("缺少配置: file_api_base_url，拒绝签发上传 token".to_string())
        })?;

    let reserved_file_id = services
        .file_service
        .reserve_file_id()
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let session_root = services
        .file_service
        .upload_session_root()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    let (session, token, expires_at) = ChunkedSession::create(
        &session_root,
        NewSession {
            uploader_id: user_id,
            total_size: request.file_size as u64,
            sealed_sha256: sha256,
            file_type: file_type.as_str().to_string(),
            business_type: request.business_type,
            filename: request
                .filename
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| "file.bin".to_string()),
            mime_type: if request.mime_type.trim().is_empty() {
                "application/octet-stream".to_string()
            } else {
                request.mime_type
            },
            transform_version: request.transform_version,
            reserved_file_id,
        },
    )
    .map_err(|e| RpcError::internal(e.to_string()))?;

    tracing::info!(
        "🎫 分片上传会话已建 upload_id={} 用户={} 大小={} 预留 file_id={}",
        session.upload_id(),
        user_id,
        request.file_size,
        reserved_file_id
    );

    let response = FileRequestChunkedUploadTokenResponse {
        already_exists: false,
        claim_token: None,
        upload_token: Some(token),
        upload_url: Some(upload_url),
        base_unit: Some(BASE_UNIT),
        expires_at: Some(expires_at),
    };
    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
