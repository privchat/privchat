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
use crate::service::chunked_upload::{
    new_session_ids, remove_s3_anchor, s3_part_geometry, select_transport, write_s3_anchor,
    ChunkedSession, NewSession, S3Anchor, S3DirectGate, S3SessionSetup, BASE_UNIT,
    TRANSPORT_S3_MULTIPART_V1,
};
use crate::service::file_service::FileService;
use crate::service::upload_token_service::UploadTokenService;
use crate::service::FileType;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::{
    FileRequestChunkedUploadTokenRequest, FileRequestChunkedUploadTokenResponse,
};

/// 签发分片上传 token 所需的窄依赖（把生产接线从 `RpcServiceContext` 里剥出来，
/// 让测试能直接驱动，见 `tests/chunked_token_negotiation_test.rs`）。
pub struct ChunkedTokenServices<'a> {
    pub file_service: &'a FileService,
    pub upload_token_service: &'a UploadTokenService,
    pub file_api_base_url: Option<&'a str>,
}

pub async fn request_chunked_upload_token(
    services: RpcServiceContext,
    params: Value,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let request: FileRequestChunkedUploadTokenRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let narrowed = ChunkedTokenServices {
        file_service: &services.file_service,
        upload_token_service: &services.upload_token_service,
        file_api_base_url: services.config.file_api_base_url.as_deref(),
    };
    let response = issue_chunked_upload_token(&narrowed, user_id, request).await?;
    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// 签发的真实接线（RESUMABLE_UPLOAD_SPEC §2.2/§8.2）：参数校验 → 🔴 协商集合校验与
/// 模式选择（**在秒传预检之前**，见下）→ 秒传 → 建会话。RPC 层只做解码与身份提取。
pub async fn issue_chunked_upload_token(
    services: &ChunkedTokenServices<'_>,
    user_id: u64,
    request: FileRequestChunkedUploadTokenRequest,
) -> RpcResult<FileRequestChunkedUploadTokenResponse> {
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

    // ---- §8.2 单一数据面模式选择（第二十轮：配置单选，禁回退/兜底）----
    // 🔴 校验在秒传预检之前：同一非法请求不会因文件是否已存在而一会儿成功一会儿失败。
    // 规则由 select_transport 自身强制（不靠注释）：配了 S3 则只认 s3_multipart_v1
    // 声明（未声明直接报「不支持该上传模式」，绝不静默改走内置服务）；没配则隐式/
    // 声明 proxy，集合存在必含 proxy_offset_v1。没有阈值、没有能力协商、没有切换。
    let declared_transports = request.supported_upload_transports.is_some();
    let s3_wiring = services.file_service.s3_direct();
    let transport = select_transport(
        request.supported_upload_transports.as_deref(),
        &S3DirectGate {
            open: s3_wiring.is_some(),
        },
    )
    .map_err(|e| match e {
        crate::service::chunked_upload::TransportSelectError::ServerS3Only => RpcError::validation(
            "不支持该上传模式：服务端已配置 S3 直传，客户端必须声明 s3_multipart_v1".to_string(),
        ),
        crate::service::chunked_upload::TransportSelectError::SetMissingProxy => {
            RpcError::validation("supported_upload_transports 必须包含 proxy_offset_v1".to_string())
        }
    })?
    .to_string();

    // ---- 2.1 秒传预检：命中就回 claim_token，不建任何目录（协商校验已在它之前）----
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
            return Ok(response);
        }
    }

    // ---- 2.2 建会话 ----
    let upload_url = services
        .file_api_base_url
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
    let filename = request
        .filename
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "file.bin".to_string());
    let mime_type = if request.mime_type.trim().is_empty() {
        "application/octet-stream".to_string()
    } else {
        request.mime_type.clone()
    };

    // ---- 2.2 建会话 ----
    // S3 直传的真实签发链路（RESUMABLE §2.2，第十六/十七轮评审）：
    // 选源/门禁在 select_transport 里已过 → 先 `CreateMultipartUpload`（对象
    // metadata 写会话 id、声明逐片 SHA256）→ 🔴 立即落盘恢复锚点 → 再写 manifest
    // （含全部冻结字段，含 `storage_source_id`）。不变式：MPU 存在 ⇒ 锚点在或
    // manifest 可读；任何一步失败都不吞掉清理失败，错误带可恢复引用。
    let (session, token, expires_at) = if transport == TRANSPORT_S3_MULTIPART_V1 {
        let wiring = s3_wiring.ok_or_else(|| {
            RpcError::internal("S3 直传门禁状态不一致：选中了 transport 但接线缺失".to_string())
        })?;
        let (part_size, total_parts) = s3_part_geometry(request.file_size as u64);
        let file_path = services
            .file_service
            .generate_file_path(&sha256, &file_type, &filename);
        let final_key = wiring.object_key(&file_path);
        let ids = new_session_ids();
        let upload_id = ids.upload_id.clone();
        let reference = wiring
            .backend
            .create(&ids.upload_id, &wiring.bucket, &final_key, request.file_size as u64)
            .await
            .map_err(|e| {
                RpcError::internal(format!("创建 S3 分片上传失败，请稍后重试: {e:?}"))
            })?;
        // 🔴 MPU 已建：恢复锚点立即落盘（第十七轮评审 P1）。之后无论 manifest 成败、
        // 进程是否崩溃，扫描器都能凭锚点找回并处置这个 MPU。
        let anchor = S3Anchor {
            bucket: wiring.bucket.clone(),
            final_key: final_key.clone(),
            provider_upload_id: reference.provider_upload_id.clone(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0) as i64,
        };
        if let Err(e) = write_s3_anchor(&session_root, &upload_id, &anchor) {
            // 锚都落不了盘：立即 abort；abort 也失败则错误必须带上可恢复引用。
            return Err(match wiring.backend.abort(&reference).await {
                Ok(()) => RpcError::internal(format!(
                    "创建 S3 分片上传会话失败（本地锚点写入失败: {e}），已清理，请重试"
                )),
                Err(ae) => {
                    tracing::error!(
                        "S3 签发：锚点写入失败且 abort 失败，桶内遗留 MPU：bucket={} key={} uploadId={}：{e} / {ae:?}",
                        wiring.bucket,
                        final_key,
                        reference.provider_upload_id
                    );
                    RpcError::internal(format!(
                        "创建 S3 分片上传会话失败且清理失败，请联系管理员（恢复信息: bucket={} key={} uploadId={}）",
                        wiring.bucket, final_key, reference.provider_upload_id
                    ))
                }
            });
        }
        let created = ChunkedSession::create_with_ids(
            &session_root,
            ids,
            NewSession {
                uploader_id: user_id,
                total_size: request.file_size as u64,
                sealed_sha256: sha256,
                file_type: file_type.as_str().to_string(),
                business_type: request.business_type,
                filename,
                mime_type,
                transform_version: request.transform_version,
                reserved_file_id,
                transport: transport.clone(),
                s3: Some(S3SessionSetup {
                    part_size,
                    total_parts,
                    bucket: wiring.bucket.clone(),
                    final_key,
                    provider_upload_id: reference.provider_upload_id.clone(),
                    storage_source_id: wiring.source_id,
                }),
            },
        );
        match created {
            Ok(triple) => {
                // manifest 可读 ⇒ 锚点使命结束；删失败只告警，锚点 GC 会收（看到
                // manifest 健在即删）。
                if let Err(e) = remove_s3_anchor(&session_root, &upload_id) {
                    tracing::warn!("S3 签发成功但删除恢复锚点失败（扫描器会收掉）: {e}");
                }
                triple
            }
            Err(e) => {
                // 🔴 §2.2：S3 调用成功但 manifest 写失败 → 先 AbortMultipartUpload。
                // 🔴 第十七轮评审 P1：abort 失败不再吞——保留锚点让扫描器重试，
                // 并把可恢复引用写进错误与日志。
                match wiring.backend.abort(&reference).await {
                    Ok(()) => {
                        let _ = remove_s3_anchor(&session_root, &upload_id);
                        return Err(RpcError::internal(format!(
                            "创建 S3 分片上传会话失败，已清理，请重试: {e}"
                        )));
                    }
                    Err(ae) => {
                        tracing::error!(
                            "S3 签发：manifest 写入失败且 abort 失败，扫描器将凭锚点重试：bucket={} key={} uploadId={}：{e} / {ae:?}",
                            wiring.bucket,
                            reference.final_key,
                            reference.provider_upload_id
                        );
                        return Err(RpcError::internal(format!(
                            "创建 S3 分片上传会话失败且清理失败，服务端将自动重试清理（恢复信息: bucket={} key={} uploadId={}）",
                            wiring.bucket, reference.final_key, reference.provider_upload_id
                        )));
                    }
                }
            }
        }
    } else {
        ChunkedSession::create(
            &session_root,
            NewSession {
                uploader_id: user_id,
                total_size: request.file_size as u64,
                sealed_sha256: sha256,
                file_type: file_type.as_str().to_string(),
                business_type: request.business_type,
                filename,
                mime_type,
                transform_version: request.transform_version,
                reserved_file_id,
                // manifest 记下协商结果：status/complete/abort 与 /files/part-url
                // 的端点强绑定（RESUMABLE §8.3）都按它分流。
                transport: transport.clone(),
                s3: None,
            },
        )
        .map_err(|e| RpcError::internal(e.to_string()))?
    };

    tracing::info!(
        "🎫 分片上传会话已建 upload_id={} 用户={} 大小={} 预留 file_id={}",
        session.upload_id(),
        user_id,
        request.file_size,
        reserved_file_id
    );

    // 只对声明了能力的客户端回协商结果；选为 S3 时另带 part_size/total_parts
    // （RESUMABLE §8.2）。旧客户端的响应不新增字段。
    let is_s3 = transport == TRANSPORT_S3_MULTIPART_V1;
    let (part_size, total_parts) = if is_s3 {
        s3_part_geometry(request.file_size as u64)
    } else {
        (0, 0)
    };
    let response = FileRequestChunkedUploadTokenResponse {
        already_exists: false,
        claim_token: None,
        upload_token: Some(token),
        upload_url: Some(upload_url),
        base_unit: Some(BASE_UNIT),
        expires_at: Some(expires_at),
        transport: declared_transports.then_some(transport),
        part_size: is_s3.then_some(part_size),
        total_parts: is_s3.then_some(total_parts),
    };
    Ok(response)
}
