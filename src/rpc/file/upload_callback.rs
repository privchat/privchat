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

//! RPC: 上传完成回调（内部 RPC）
//!
//! 由文件服务器调用，通知业务服务器文件上传完成

use serde_json::{json, Value};
use tracing::warn;

use crate::rpc::{RpcError, RpcResult, RpcServiceContext};

/// 上传完成回调
pub async fn upload_callback(services: RpcServiceContext, params: Value) -> RpcResult<Value> {
    // 解析参数
    let upload_token = params
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("缺少 token 参数".to_string()))?;

    let file_id = params["file_id"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_id 参数".to_string()))?
        .to_string();

    let _file_url = params["file_url"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 file_url 参数".to_string()))?
        .to_string();

    let _thumbnail_url = params["thumbnail_url"].as_str().map(|s| s.to_string());

    let file_size = params["file_size"]
        .as_u64()
        .ok_or_else(|| RpcError::validation("缺少 file_size 参数".to_string()))?;

    let _original_size = params["original_size"].as_u64();

    let _mime_type = params["mime_type"]
        .as_str()
        .ok_or_else(|| RpcError::validation("缺少 mime_type 参数".to_string()))?
        .to_string();

    let width = params["width"].as_u64().map(|v| v as u32);
    let height = params["height"].as_u64().map(|v| v as u32);

    tracing::debug!(
        "📤 文件上传完成回调: file_id={}, size={} bytes",
        file_id,
        file_size
    );

    // 🔴 **token 仍然有效是正常的，不是异常。**
    //
    // 这里原本把「token 还能验过」当告警、随后 `remove_token`。两条语义都建立在
    // 「一次性 5 分钟 token」上：现在 token 最长 24 小时、可复用，而且签名 token
    // 服务端根本不存储，**没有东西可删**。
    //
    // 所谓「完成后清理 token」，实际含义是清理上传临时数据；token 到期自行失效。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Err(e) = services
        .upload_token_service
        .validate_any(now_secs, upload_token)
        .await
    {
        warn!("⚠️ 上传回调携带的 token 无效: {e}");
    }

    // TODO: 记录文件元数据到数据库
    // TODO: 更新用户配额
    // TODO: 触发后续业务逻辑（如媒体处理、内容审核）

    Ok(json!({
        "success": true,
        "message": "文件上传成功",
    }))
}
