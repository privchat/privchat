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

use privchat_protocol::rpc::file::upload::FileGetUrlRequest;
use privchat_protocol::rpc::file::upload::FileGetUrlResponse;
use serde_json::Value;

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcContext;
use crate::rpc::RpcServiceContext;

/// 处理获取文件 URL 请求
pub async fn get_file_url(
    services: RpcServiceContext,
    params: Value,
    _ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&_ctx)?;

    let request: FileGetUrlRequest =
        serde_json::from_value(params).map_err(|e| RpcError::validation(format!("参数错误: {}", e)))?;

    tracing::info!("🔗 获取文件 URL: file_id={}, user_id={}", request.file_id, user_id);

    // 附件访问授权（ATTACHMENT_ENCRYPTION_SPEC §授权）：本接口返回 CEK，必须校验访问权，
    // 否则任意登录用户拿 file_id 即可解密。方案 A + pending uploader-only fallback：
    //  - business_id 已绑定 message → file→message→channel 成员校验
    //  - business_id 未绑定（pending）→ 仅 uploader
    //  - 任何异常（消息不存在/查询失败）→ 拒绝，不 fallback 放行
    // 注意：cek 绝不进日志。
    let file_meta = services
        .file_service
        .get_file_metadata(request.file_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询文件失败: {}", e)))?
        .ok_or_else(|| RpcError::validation("文件不存在".to_string()))?;

    let bound_message_id = file_meta
        .business_id
        .as_deref()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|id| *id > 0);

    let authorized = match bound_message_id {
        Some(message_id) => match services.message_repository.get_channel_id(message_id).await {
            Ok(Some(channel_id)) => services
                .channel_service
                .is_channel_member(channel_id, user_id)
                .await
                .unwrap_or(false),
            // 消息不存在 / channel 缺失 / 查询失败 → 拒绝
            _ => false,
        },
        // pending：未绑定消息，只允许上传者本人（发送端预览/重试）
        None => file_meta.uploader_id == user_id,
    };

    if !authorized {
        tracing::warn!(
            "🚫 拒绝访问附件: file_id={}, user_id={}, bound_message={:?}",
            request.file_id,
            user_id,
            bound_message_id
        );
        return Err(RpcError::forbidden("无权访问该附件".to_string()));
    }

    let url = services
        .file_service
        .get_file_url(request.file_id, user_id)
        .await
        .map_err(|e| RpcError::internal(format!("获取文件 URL 失败: {}", e)))?;

    tracing::info!("🔗 返回文件 URL: {}", url.file_url);

    let response = FileGetUrlResponse {
        file_url: url.file_url,
        expires_at: url.expires_at,
        file_size: url.file_size as u64,
        mime_type: url.mime_type,
        encryption_version: url.encryption_version,
        cek: url.cek,
    };

    Ok(serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))?)
}
