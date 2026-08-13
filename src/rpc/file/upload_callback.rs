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

    // 🔴 **token 仍然有效是正常的，不是异常** —— 但无效必须拒绝。
    //
    // 这里原本把「token 还能验过」当告警、随后 `remove_token`，而验证失败只记一条
    // warning 就照样返回成功。两条都不对：前者的语义建立在「一次性 5 分钟 token」上
    //（现在 token 最长 24 小时、可复用，签名 token 服务端根本不存储，没有东西可删）；
    // 后者等于**任何无效 token 都能拿到成功回调**。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let token_info = services
        .upload_token_service
        .validate_any(now_secs, upload_token)
        .await
        .map_err(|e| {
            warn!(
                "❌ 上传回调 token 无效: {}… ({e})",
                upload_token.chars().take(8).collect::<String>()
            );
            RpcError::validation("上传 token 无效".to_string())
        })?;

    // claim 用途的 token 换不出「我传完了」这件事。
    if token_info.purpose
        != crate::service::upload_token_service::UploadTokenPurpose::Upload
    {
        return Err(RpcError::validation(
            "该 token 用于秒传取用，不能用作上传完成回调".to_string(),
        ));
    }

    // 🔴 `file_id` 必须**确实是这次上传的结果**，不能由调用方随口报一个。
    // 判据是完成幂等键：它由 `upload_id` 派生，且与文件行同事务写入。
    let completion_key = {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(token_info.upload_id.as_bytes()))
    };
    let file_id_num: u64 = file_id
        .parse()
        .map_err(|_| RpcError::validation("file_id 不是合法数字".to_string()))?;
    match services
        .file_service
        .find_completed_upload(token_info.user_id, &completion_key)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
    {
        Some(expected) if expected == file_id_num => {}
        Some(expected) => {
            warn!(
                "❌ 上传回调 file_id 与该 upload_id 的完成结果不符: 报 {file_id_num}，实为 {expected}"
            );
            return Err(RpcError::validation(
                "file_id 与该次上传不符".to_string(),
            ));
        }
        None => {
            return Err(RpcError::validation(
                "该次上传尚未完成，无法回调".to_string(),
            ));
        }
    }

    // TODO: 记录文件元数据到数据库
    // TODO: 更新用户配额
    // TODO: 触发后续业务逻辑（如媒体处理、内容审核）

    Ok(json!({
        "success": true,
        "message": "文件上传成功",
    }))
}
