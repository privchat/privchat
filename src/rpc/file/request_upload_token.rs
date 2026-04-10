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

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};
use crate::service::FileType;
use privchat_protocol::rpc::{FileRequestUploadTokenRequest, FileRequestUploadTokenResponse};

/// 请求上传 token
pub async fn request_upload_token(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // ✨ 使用协议层类型自动反序列化
    let request: FileRequestUploadTokenRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = request.user_id;
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
        return Err(RpcError::validation(format!(
            "文件大小超过限制（最大 {} MB）",
            max_size / 1024 / 1024
        )));
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
        .map(|base_url| format!("{}/api/app/files/upload", base_url.trim_end_matches('/')))
        .ok_or_else(|| {
            RpcError::internal("缺少配置: file_api_base_url，拒绝签发上传 token".to_string())
        })?;

    let response = FileRequestUploadTokenResponse {
        token: token.token.clone(),
        upload_url,
        // request_upload_token 阶段尚未落盘具体文件，保留兼容空值
        file_id: String::new(),
        expires_at: Some(token.expires_at.timestamp()),
        max_size: Some(token.max_size),
    };

    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// 根据文件类型获取最大文件大小限制
fn get_max_size_for_type(file_type: &FileType) -> i64 {
    match file_type {
        FileType::Image => 10 * 1024 * 1024,  // 10 MB
        FileType::Video => 200 * 1024 * 1024, // 200 MB
        FileType::Audio => 50 * 1024 * 1024,  // 50 MB
        FileType::File => 100 * 1024 * 1024,  // 100 MB
        FileType::Other => 50 * 1024 * 1024,  // 50 MB
    }
}
