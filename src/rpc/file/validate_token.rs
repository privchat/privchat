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

    // 🔴 日志不落完整 bearer token。
    tracing::debug!(
        "🔐 验证上传 token: {}…",
        upload_token.chars().take(8).collect::<String>()
    );

    // 🔴 走统一入口：只用 `validate_token` 的话，signed 模式下这个 RPC 恒失败
    //（那条路只认 Redis 里的 UUID）。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match services
        .upload_token_service
        .validate_any(now_secs, upload_token)
        .await
    {
        Ok(token_info) => {
            // 🔴 **只校验，不消费。**
            //
            // 这里原本顺手 `mark_token_used`。5 分钟一次性 token 时代无所谓，
            // 24 小时可复用 token 下等于「任何人调一次校验就废掉别人整个上传」。
            //
            // 重放不靠烧 token，由这几样承担，**都不在业务库里**：
            // 会话模式锁（同一 upload_id 只允许一条路径且整包接收期间独占）、
            // `reserved_file_id`（重试复用同一个 id）、墓碑（已完成就直接返回原结果）、
            // 以及落库时 `file_id` 主键冲突收敛。
            Ok(json!({
                "valid": true,
                "user_id": token_info.user_id,
                "file_type": token_info.file_type.as_str(),
                "max_size": token_info.max_size,
                "business_type": token_info.business_type,
            }))
        }
        Err(e) => {
            warn!(
                "❌ Token 验证失败: {}… ({e})",
                upload_token.chars().take(8).collect::<String>()
            );
            Ok(json!({ "valid": false, "reason": e.to_string() }))
        }
    }
}
