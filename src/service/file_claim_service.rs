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

//! 秒传取用的领域逻辑：`file/claim_existing` 的全部判定都在这里。
//!
//! 抽出来是为了**它本身可以被直接测**。放在 handler 里的话，要跑到它得先造出
//! `RpcServiceContext` 的三十多个服务；于是测试只能退到仓储层，而
//! 「幂等查询有没有排在 token 校验之前」这种顺序问题，仓储层根本看不见——
//! 有人把那一步挪到后面，测试照样绿。
//!
//! 这里只依赖两样东西：文件服务和上传 token 服务。

use std::sync::Arc;

use crate::error::{Result, ServerError};
use crate::model::file_upload::FileMetadata;
use crate::service::upload_token_service::{UploadTokenPurpose, UploadTokenService};
use crate::service::FileService;

/// token 的摘要，作为幂等键。
pub fn claim_key_hash(token: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 用一张 claim token 换到**自己的**那条文件记录。
///
/// 重复调用返回同一条：数据库提交了但响应丢了，客户端拿同一个 token 重试，
/// 拿到的是同一个 `file_id`，而不是又多一行。
#[allow(clippy::too_many_arguments)]
pub async fn claim_existing_file(
    file_service: &Arc<FileService>,
    token_service: &Arc<UploadTokenService>,
    // 判定「这个人现在还能不能读这份文件」用的是 get_url 那一套，不另立判据。
    message_repository: &crate::repository::message_repo::PgMessageRepository,
    channel_service: &crate::service::channel_service::ChannelService,
    user_id: u64,
    token_str: &str,
    request_sha256: &str,
) -> Result<FileMetadata> {
    let key = claim_key_hash(token_str);

    // 🔴 幂等查询必须排在 token 校验**之前**。成功过的 token 已经被消费掉，
    // 先去校验只会得到「无效」，重试就永远拿不回那条记录。
    if let Some(existing) = file_service.find_claimed(user_id, &key).await? {
        if let Some(meta) = file_service.get_file_metadata(existing).await? {
            return Ok(meta);
        }
    }

    let token = token_service
        .validate_token(token_str)
        .await
        .map_err(|_| ServerError::Validation("上传 token 无效或已过期".to_string()))?;

    if token.purpose != UploadTokenPurpose::ClaimExisting {
        return Err(ServerError::Validation(
            "该 token 用于实体上传，不能用于秒传取用".to_string(),
        ));
    }
    if token.user_id != user_id {
        return Err(ServerError::Forbidden(
            "上传 token 不属于当前用户".to_string(),
        ));
    }

    // 逐项复核 token 里签下的身份，不信这次请求带来的参数——
    // 否则可以 prepare 一个小文件、claim 另一份内容。
    let bound = token
        .sha256
        .as_deref()
        .ok_or_else(|| ServerError::Validation("该 token 未绑定内容摘要".to_string()))?;
    if !bound.eq_ignore_ascii_case(request_sha256.trim()) {
        return Err(ServerError::Validation(
            "sha256 与 token 绑定的内容不一致".to_string(),
        ));
    }

    // 脏摘要不会立刻报错，只会让秒传永远命不中，表现成「怎么每次都重传」，
    // 很难往回查——所以在入口就拒掉。
    let normalized = bound.trim().to_ascii_lowercase();
    if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(ServerError::Validation(
            "sha256 必须是 64 位十六进制（SHA-256）".to_string(),
        ));
    }

    // 判重只看摘要：字节相同就是同一份东西。
    //
    // 🔴 同一串字节可能已经有好几条记录（alice 发在群 A、bob 发在群 B……）。
    // 物理文件一份，记录多条，而**授权是按记录算的**：调用者有权读的未必是最老
    // 那条。所以逐条试，取第一条他现在读得到的，不能先钉死一条再判。
    let candidates = file_service.find_all_by_content(&normalized).await?;
    let mut source = None;
    for candidate in candidates {
        let decision = crate::service::attachment_authorization::resolve_attachment_access(
            message_repository,
            channel_service,
            &candidate,
            user_id,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                "秒传授权判定不可用 source_file_id={} user_id={}: {}",
                candidate.file_id,
                user_id,
                error
            );
            ServerError::Internal("ATTACHMENT_AUTHORIZATION_UNAVAILABLE".to_string())
        })?;
        if decision.authorized {
            source = Some(candidate);
            break;
        }
    }
    // 🔴 与「服务端没有这份内容」返回同一句话。分开说的话，这个接口就成了
    // 文件存在性探测器：拿一堆摘要来问，能区分「没有」和「有但你无权」。
    let source = source.ok_or_else(|| {
        ServerError::NotFound("服务端没有这份内容，请正常上传".to_string())
    })?;

    let file_id = file_service
        .copy_for_user(&source, user_id, &token.business_type, Some(&key))
        .await?;

    // 🔴 消费 token 放在数据库提交**之后**。放前面的话，后续任何失败都会把 token
    // 烧掉，客户端连重试的机会都没有；幂等已经由 claim_key_hash 保证，
    // 这里消费失败也不会多出一行。
    if let Err(e) = token_service.mark_token_used(token_str).await {
        tracing::warn!("标记上传 token 已使用失败（幂等由 claim_key_hash 保证）: {e}");
    }

    // 🔴 按新 `file_id` **重新读库**，而不是克隆源行改个 id。
    //
    // 克隆出来的对象带着原上传者的 `uploader_id`、`business_id`、`uploaded_at`，
    // 那不是新用户那一行。当前 RPC 恰好没下发这几个字段，所以看不出问题——
    // 但领域对象一旦被别处拿去判归属，错的就是权限。
    file_service
        .get_file_metadata(file_id)
        .await?
        .ok_or_else(|| ServerError::Internal(format!("秒传记录 {file_id} 刚写入却读不到")))
}
