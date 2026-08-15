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
    // 🔴 只有上限是不够的：`file_size = 0` 会让完成时的「声明 vs 实际」复核
    // 退化成与 0 比对，等于没校验。
    if file_size <= 0 {
        return Err(RpcError::validation(
            "file_size 必须大于 0".to_string(),
        ));
    }
    // 🔴 文件名要签进 token，而 token 跟着每一个分片请求走。
    // `original_filename` 列是 VARCHAR(512)，真放进来会把 token 撑破体积预算。
    if let Some(name) = filename.as_ref() {
        if name.len() > crate::security::upload_token::MAX_FILENAME_BYTES {
            return Err(RpcError::validation(format!(
                "文件名过长（最多 {} 字节）",
                crate::security::upload_token::MAX_FILENAME_BYTES
            )));
        }
    }

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

    // 秒传探测：**只回答「这份内容在不在」**，不产生任何副作用。
    //
    // 🔴 prepare 绝不建句柄。反复探测会攒出一堆没有任何消息使用的孤儿记录；
    // 而且「探测」和「取得所有权」是两件事，混在一个接口里，任何一次重试
    // 都会多给调用方一份文件。命中之后由客户端另外调 `file/claim_existing`
    // 带 token + sha256 去换**他自己的** file_id。
    let mut already_exists = false;
    if let Some(sha256) = request.sha256.as_deref() {
        let normalized = sha256.trim().to_ascii_lowercase();
        if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(RpcError::validation(
                "sha256 必须是 64 位十六进制（SHA-256）".to_string(),
            ));
        }
        // 📌 签进 token 的规范化由 `UploadTokenService::issue()` 统一负责
        //（放在这里的话，将来多一条签发入口就会漏）。这里的 `normalized` 只用于查库。
        already_exists = services
            .file_service
            .find_by_content(&normalized)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
            .is_some();
    }

    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // 🔴 整包接口**不再下发分片方案**：分片走独立 RPC `file/request_chunked_upload_token`
    //（RESUMABLE_UPLOAD_SPEC §2）。靠"响应里有没有 upload_plan"暗示走哪条路的那版
    // 已作废——它让分片路径静默退化成整包而没有任何报错。
    let upload_plan: Option<crate::service::upload_token_service::UploadPlan> = None;

    let (token_str, _upload_id, expires_at) = services
        .upload_token_service
        .issue(
            now_secs,
            user_id,
            file_type,
            max_size,
            business_type,
            filename,
            // 文件身份签进 token，完成时逐项复核——否则客户端可以在 prepare 与
            // upload/claim 之间把摘要或大小换掉。
            crate::service::upload_token_service::UploadIdentity {
                sha256: request.sha256.clone(),
                declared_size: Some(file_size),
                mime_type: Some(mime_type.clone()),
                transform_version: request.transform_version,
            },
            // 命中 → 这张 token 只能拿去 claim；未命中 → 只能拿去传字节。
            // 两个入口各自拒绝不属于自己的用途，堵住「同一张 token 两边各用一次」。
            if already_exists {
                crate::service::upload_token_service::UploadTokenPurpose::ClaimExisting
            } else {
                crate::service::upload_token_service::UploadTokenPurpose::Upload
            },
            upload_plan.as_ref(),
        )
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
        token: token_str,
        upload_url,
        // 命中只是**告知**：字节已经在服务端，不必再传。要拿到自己的 file_id
        // 还得带这个 token 调 `file/claim_existing`。
        already_exists,
        // request_upload_token 阶段尚未落盘具体文件，保留兼容空值
        file_id: String::new(),
        expires_at: Some(expires_at),
        max_size: Some(max_size),
        // 客户端据此决定分片还是整包。同一份方案已经签进 token，服务端按它校验每个
        // 分片请求——这里给的是**同一件事的可读副本**，不是第二个真源。
        upload_plan: upload_plan.as_ref().map(|p| {
            privchat_protocol::rpc::file::UploadPlanDto {
                base_unit: p.base_unit,
                initial_request_size: p.initial_request_size,
                max_request_size: p.max_request_size,
                session_threshold: p.session_threshold,
                max_parallel_parts: p.max_parallel_parts,
            }
        }),
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
