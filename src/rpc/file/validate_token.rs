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

//! RPC: 验证上传 token（内部 RPC）
//!
//! 由文件服务器调用，验证上传 token 的有效性

use serde_json::{json, Value};
use tracing::warn;

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};

/// 验证上传 token
pub async fn validate_upload_token(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // 解析参数
    let upload_token = params
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("缺少 token 参数".to_string()))?;

    tracing::debug!("🔐 验证上传 token: {}", upload_token);

    // 验证 token
    match services
        .upload_token_service
        .validate_token(upload_token)
        .await
    {
        Ok(token_info) => {
            // Token 有效，标记为已使用
            services
                .upload_token_service
                .mark_token_used(upload_token)
                .await
                .map_err(|e| RpcError::internal(e.to_string()))?;

            Ok(json!({
                "valid": true,
                "user_id": token_info.user_id,
                "file_type": token_info.file_type.as_str(),
                "max_size": token_info.max_size,
                "business_type": token_info.business_type,
            }))
        }
        Err(e) => {
            warn!("❌ Token 验证失败: {}", upload_token);
            Ok(json!({
                "valid": false,
                "error": e.to_string(),
            }))
        }
    }
}
