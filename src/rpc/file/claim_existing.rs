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

    // 🔴 幂等第一步：这个 token 之前成功取用过吗？
    //
    // 命中就把当时那个 file_id 原样还回去。数据库提交了但响应丢了的情况，
    // 客户端重试拿到的是同一份，而不是又多一行——而且这一步在 token 校验之前，
    // 因为成功过的 token 已经被消费掉了，再去校验只会得到「无效」。
    let claim_key_hash = {
        use sha2::Digest as _;
        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        hasher.update(token_str.as_bytes());
        hex::encode(hasher.finalize())
    };
    if let Some(existing) = services
        .file_service
        .find_claimed(user_id, &claim_key_hash)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
    {
        if let Some(meta) = services
            .file_service
            .get_file_metadata(existing)
            .await
            .map_err(|e| RpcError::internal(e.to_string()))?
        {
            return upload_result(&services, &meta);
        }
    }

    // token 必须有效、属于当前用户、且未被用过（一次性）。
    let token = services
        .upload_token_service
        .validate_token(token_str)
        .await
        .map_err(|_| RpcError::validation("上传 token 无效或已过期".to_string()))?;

    if token.purpose != crate::service::upload_token_service::UploadTokenPurpose::ClaimExisting {
        return Err(RpcError::validation(
            "该 token 用于实体上传，不能用于秒传取用".to_string(),
        ));
    }

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

    // 摘要必须是 64 位十六进制。脏摘要不会立刻报错，只会让秒传永远命不中，
    // 表现成「怎么每次都重传」，很难往回查——所以在入口就拒掉。
    let normalized = bound.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::validation(
            "sha256 必须是 64 位十六进制（SHA-256）".to_string(),
        ));
    }

    // 判重只看摘要：字节相同就是同一份东西。
    let source = services
        .file_service
        .find_by_content(&normalized)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?
        .ok_or_else(|| RpcError::not_found("服务端没有这份内容，请正常上传".to_string()))?;

    // 照着已有那行给**当前用户**插一条新记录：物理文件一份，两行指向它。
    // 他拿到的是自己的 file_id，绑到自己的消息上，与别人那条消息毫无关系。
    let file_id = services
        .file_service
        .copy_for_user(&source, user_id, &token.business_type, Some(&claim_key_hash))
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    // 🔴 消费 token 放在**数据库提交之后**。放前面的话，后续任何失败都会把 token
    // 烧掉，客户端连重试的机会都没有；而幂等已经由 claim_key_hash 保证，
    // 这里消费失败也不会多出一行。
    if let Err(e) = services.upload_token_service.mark_token_used(token_str).await {
        tracing::warn!("标记上传 token 已使用失败（幂等由 claim_key_hash 保证）: {e}");
    }

    tracing::info!(
        "⚡ 秒传取用: user={} 复用 path={} → file_id={}",
        user_id,
        source.file_path,
        file_id
    );

    upload_result(&services, &source_after_claim(&source, file_id))
}

/// 秒传取用的结果，形状与 `/files/upload` 的 `UploadResponse` **逐字段一致**。
///
/// 客户端两条路径拿到的应该是同一种东西——差一个字段，调用方就得写两套解析，
/// 而那两套迟早会分叉。
fn upload_result(
    services: &RpcServiceContext,
    meta: &crate::model::file_upload::FileMetadata,
) -> RpcResult<Value> {
    Ok(json!({
        "file_id": meta.file_id,
        "file_url": services
            .file_service
            .build_access_url(&meta.file_path, meta.storage_source_id),
        "thumbnail_url": serde_json::Value::Null,
        "file_size": meta.file_size,
        "original_size": meta.original_size,
        "width": meta.width,
        "height": meta.height,
        "mime_type": meta.mime_type,
        "uploaded_at": chrono::Utc::now().timestamp_millis(),
        "storage_source_id": meta.storage_source_id,
    }))
}

/// 取用产生的那一行：内容字段沿用源行，`file_id` 换成自己的。
fn source_after_claim(
    source: &crate::model::file_upload::FileMetadata,
    file_id: u64,
) -> crate::model::file_upload::FileMetadata {
    let mut meta = source.clone();
    meta.file_id = file_id;
    meta
}
