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

//! 秒传命中后的**取得所有权**这一步：`file/claim_existing`。
//!
//! 上传流程被明确拆成两件事，不许合并：
//!
//! ```text
//! file/request_upload_token   探测：这份内容在不在？（无副作用）
//!   already_exists = false → 照常上传字节，走 /files/upload 完成
//!   already_exists = true  → file/claim_existing（本文件）换自己的 file_id
//! ```
//!
//! 🔴 拆开的理由是「探测」会被重试。放在一起的话，每探测一次就多给调用方
//! 一份文件记录，攒出一堆没有任何消息使用的孤儿句柄。
//!
//! 两条路径最终都产出**当前用户自己的** `file_id`，形状一致；后续发消息就是
//! 普通的 image / video / file，没有任何秒传专用分支。

use serde_json::{json, Value};

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{RpcContext, RpcServiceContext};

pub async fn claim_existing(
    services: RpcServiceContext,
    params: Value,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let token_str = params
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("token is required".to_string()))?;
    let sha256 = params
        .get("sha256")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("sha256 is required".to_string()))?;

    // token 必须有效、属于当前用户、且未被用过（一次性）。
    let token = services
        .upload_token_service
        .validate_token(token_str)
        .await
        .map_err(|_| RpcError::validation("上传 token 无效或已过期".to_string()))?;

    if token.user_id != user_id {
        return Err(RpcError::forbidden("上传 token 不属于当前用户".to_string()));
    }

    // 🔴 逐项复核 token 里签下的身份，不信这次请求带来的参数。
    // 否则客户端可以 prepare 一个小文件、claim 另一份内容。
    let bound = token
        .sha256
        .as_deref()
        .ok_or_else(|| RpcError::validation("该 token 未绑定内容摘要".to_string()))?;
    if !bound.eq_ignore_ascii_case(sha256.trim()) {
        return Err(RpcError::validation(
            "sha256 与 token 绑定的内容不一致".to_string(),
        ));
    }

    let identity = crate::service::media_blob_service::BlobIdentity::parse(bound)
        .map_err(|e| RpcError::validation(e.to_string()))?;

    let pool = services.channel_service.pool();
    let blob = crate::service::media_blob_service::find_blob(pool, &identity)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(|| RpcError::not_found("服务端没有这份内容，请正常上传".to_string()))?;

    // 命中 ≠ 放行。只凭摘要就发句柄，等于「知道 hash 就等于拥有这个文件」。
    // 判据是他**已经有权读到这份内容**（自己传过，或能读到引用它的消息），
    // 用的是 `file/get_url` 的同一个决策函数。
    let may_reuse = crate::service::media_blob_service::may_reuse(
        pool,
        &services.message_repository,
        &services.channel_service,
        blob.blob_id,
        user_id,
    )
    .await
    .map_err(|e| RpcError::internal(e.to_string()))?;
    if !may_reuse {
        return Err(RpcError::forbidden(
            "无权复用该文件，请正常上传".to_string(),
        ));
    }

    let file_id = services
        .file_service
        .create_handle_for_blob(
            &blob,
            user_id,
            token.filename.as_deref().unwrap_or("file.bin"),
            token.file_type.as_str(),
            &token.business_type,
        )
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    // token 一次性：换过一次就作废，重试拿不到第二份句柄。
    services
        .upload_token_service
        .mark_token_used(token_str)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    tracing::info!(
        "⚡ 秒传取用: user={} blob={} → file_id={}",
        user_id,
        blob.blob_id,
        file_id
    );

    // 形状与正常上传的结果一致：客户端两条路径拿到的是同一种东西。
    Ok(json!({
        "file_id": file_id.to_string(),
        "file_size": blob.file_size,
        "mime_type": blob.mime_type,
        "sha256": identity.content_sha256,
        "encryption_version": blob.encryption_version,
    }))
}
