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

    let meta = crate::service::file_claim_service::claim_existing_file(
        &services.file_service,
        &services.upload_token_service,
        user_id,
        token_str,
        sha256,
    )
    .await
    .map_err(RpcError::from)?;

    tracing::info!(
        "⚡ 秒传取用: user={} 复用 path={} → file_id={}",
        user_id,
        meta.file_path,
        meta.file_id
    );

    upload_result(&services, &meta)
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

