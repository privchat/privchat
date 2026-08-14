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
    //
    // 判据取自**临时会话**（`state.json` 的墓碑），不查业务库：上传中间态不进
    // PostgreSQL。会话已被清理或丢失时，这次回调就没有可核对的依据，直接拒绝——
    // 客户端重新申请 token 从头传即可。
    let file_id_num: u64 = file_id
        .parse()
        .map_err(|_| RpcError::validation("file_id 不是合法数字".to_string()))?;
    let session_root = services
        .file_service
        .upload_session_root()
        .map_err(|e| RpcError::internal(e.to_string()))?;
    // 🔴 `open_existing`：恢复类入口不得惰性建目录。会话没了就是没了
    //（`SessionGone` 语义），不该被伪装成「有一个空会话」，也不该留下垃圾目录。
    let session = crate::service::upload_session::UploadSession::open_existing(
        &session_root,
        token_info.user_id,
        &token_info.upload_id,
    )
    .map_err(|e| RpcError::internal(e.to_string()))?
    .ok_or_else(|| RpcError::validation("该次上传的会话已不存在".to_string()))?;
    match session
        .completed_file_id()
        .map_err(|e| RpcError::internal(e.to_string()))?
    {
        Some(expected) if expected == file_id_num => {}
        Some(expected) => {
            warn!("❌ 上传回调 file_id 与该次上传的结果不符: 报 {file_id_num}，实为 {expected}");
            return Err(RpcError::validation("file_id 与该次上传不符".to_string()));
        }
        None => {
            return Err(RpcError::validation(
                "该次上传尚未完成，无法回调".to_string(),
            ));
        }
    }

    // 🔴 墓碑只**定位**，不构成授权：还要回读正式文件行，用与 HTTP 幂等出口、
    // 预留恢复**同一个**判据核对身份。少了这一步，一个被篡改或串了的墓碑就能让
    // 回调对着别人的文件生效。
    let meta = services
        .file_service
        .get_file_metadata(file_id_num)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(|| RpcError::validation("file_id 不存在".to_string()))?;
    if !token_info.matches_file(&meta) {
        warn!("❌ 上传回调 file_id={file_id_num} 与 token 冻结的身份不符");
        return Err(RpcError::validation(
            "file_id 与本次上传的身份不符".to_string(),
        ));
    }

    // TODO: 记录文件元数据到数据库
    // TODO: 更新用户配额
    // TODO: 触发后续业务逻辑（如媒体处理、内容审核）

    Ok(json!({
        "success": true,
        "message": "文件上传成功",
    }))
}
