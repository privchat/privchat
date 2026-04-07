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
    };

    Ok(serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化失败: {}", e)))?)
}
