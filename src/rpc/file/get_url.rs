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

    let request: FileGetUrlRequest = serde_json::from_value(params)
        .map_err(|e| RpcError::validation(format!("参数错误: {}", e)))?;

    tracing::info!(
        "🔗 获取文件 URL: file_id={}, user_id={}",
        request.file_id,
        user_id
    );

    // 附件访问授权（MEDIA_REFERENCE_AND_FORWARD_SPEC §4.1）：本接口返回 CEK，
    // 必须校验访问权，否则任意登录用户拿 file_id 即可解密。注意：cek 绝不进日志。
    //
    // 判据是**存在性**，不是单点绑定：只要存在一条引用该文件、且未删除未撤回的消息，
    // 请求者又是那条消息所在会话的成员，就放行。转发副本靠的就是这一条。
    //
    // 候选消息有两条发现路径，**判据只有一套**：
    //   1. 引用表（权威）
    //   2. 老的 business_id 单点绑定（过渡期，存量回填前的兜底）
    // 🔴 第 2 条只是「怎么找到候选消息」的另一种方式，**不是**回落到旧的
    // authorize_file_access 语义——否则 §4.2 那个「撤回后附件仍可下载」的洞会原样留着。
    let file_meta = services
        .file_service
        .get_file_metadata(request.file_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询文件失败: {}", e)))?
        .ok_or_else(|| RpcError::validation("文件不存在".to_string()))?;

    let decision = crate::service::attachment_authorization::resolve_attachment_access(
        &services.message_repository,
        &services.channel_service,
        &file_meta,
        user_id,
    )
    .await;

    if decision.authorized {
        // fallback 命中率是「回填够不够」的唯一读数。归零之前不能删掉第 2 条发现路径，
        // 归零之后才谈得上移除 business_id 兼容（spec §10 第 9 步）。
        crate::infra::metrics::record_file_access_authorized(matches!(
            decision.source,
            crate::service::attachment_authorization::CandidateSource::LegacyBusinessId
        ));
    } else {
        tracing::warn!(
            "🚫 拒绝访问附件: file_id={}, user_id={}, candidates={}, source={:?}",
            request.file_id,
            user_id,
            decision.candidate_count,
            decision.source
        );
        crate::infra::metrics::record_file_access_denied();
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
        // 文件名取自 file 表（已在上方鉴权时拉取的 file_meta），统一由 get_url 下发。
        original_filename: file_meta.original_filename,
        encryption_version: url.encryption_version,
        cek: url.cek,
    };

    Ok(serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))?)
}
