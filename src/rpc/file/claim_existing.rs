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

    // 摘要必须是 64 位十六进制。脏摘要不会立刻报错，只会让秒传永远命不中，
    // 表现成「怎么每次都重传」，很难往回查——所以在入口就拒掉。
    let normalized = bound.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RpcError::validation(
            "sha256 必须是 64 位十六进制（SHA-256）".to_string(),
        ));
    }

    // 🔴 token 一次性：**先消费再插入**。
    //
    // 原来的顺序是「插入用户文件行 → 标记 token 已使用」，两个并发 claim 会各插一行。
    // 消费失败（已被别人用掉）就直接返回，不再往下走。
    services
        .upload_token_service
        .mark_token_used(token_str)
        .await
        .map_err(|_| RpcError::validation("上传 token 已被使用".to_string()))?;

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
        .copy_for_user(&source, user_id, &token.business_type)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;

    tracing::info!(
        "⚡ 秒传取用: user={} 复用 path={} → file_id={}",
        user_id,
        source.file_path,
        file_id
    );

    // 🔴 形状必须与 `/files/upload` 的 `UploadResponse` **逐字段一致**。
    // 客户端两条路径拿到的应该是同一种东西——差一个字段，调用方就得写两套解析，
    // 而那两套迟早会分叉。
    Ok(json!({
        "file_id": file_id,
        "file_url": services
            .file_service
            .build_access_url(&source.file_path, source.storage_source_id),
        "thumbnail_url": serde_json::Value::Null,
        "file_size": source.file_size,
        "original_size": source.original_size,
        "width": source.width,
        "height": source.height,
        "mime_type": source.mime_type,
        "uploaded_at": chrono::Utc::now().timestamp_millis(),
        "storage_source_id": source.storage_source_id,
    }))
}
