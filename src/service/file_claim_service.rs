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
///
/// 🔴 **不接收 message_repository / channel_service**。摘要即持有证明之后它们不再参与
/// 判断，留着签名里就是给"哪天把记录授权接回来"留了条路——而那条路会让跨用户秒传
/// 再次失效，并且不会有任何测试发现。
pub async fn claim_existing_file(
    file_service: &Arc<FileService>,
    token_service: &Arc<UploadTokenService>,
    user_id: u64,
    token_str: &str,
    request_sha256: &str,
) -> Result<FileMetadata> {
    let key = claim_key_hash(token_str);

    // 🔴 幂等查询排在 token 校验**之前**：已经成功过的 claim，结果优先返回。
    //
    // 覆盖两种正常情况：**响应丢了**客户端拿同一张 token 重试；以及**token 已过期**
    // （24 小时到了）才来重试——先校验的话后者只会得到「无效」，那条已经属于他的
    // 记录就永远取不回来了。
    //
    // 幂等键由收到的凭证派生，查询按 `uploader_id` 限定，所以这条捷径只能取回
    // **自己**那次 claim 的结果。
    if let Some(existing) = file_service.find_claimed(user_id, &key).await? {
        if let Some(meta) = file_service.get_file_metadata(existing).await? {
            return Ok(meta);
        }
    }

    // 🔴 走**统一入口**。只用 `validate_token` 的话它仅认 Redis 里的 UUID：
    // 一旦 `issue_mode` 切到 signed，预检命中签出的是 signed claim token，
    // 这里当场判「token 无效」——**秒传全线断掉**。
    // 迁移期漏掉一个验证点，效果和没迁一样。
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let token = token_service
        .validate_any(now_secs, token_str)
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

    // 🔴 **持有明文摘要即视为持有该内容**——这是有意的取舍，不是遗漏。
    //
    // 原来的实现要求申请者对**已有的某条记录**有访问权（逐条试授权，取第一条他读得到
    // 的）。那条规则让跨用户秒传根本不成立：两个互不相识的人发同一份文件，第二个人对
    // 第一个人的记录当然没有访问权，于是 claim 必然失败、退回整传。省不下带宽，也做
    // 不到「同一份文件只存一份」，与 spec 承诺的「第二个人零字节上传」直接矛盾。
    //
    // 代价说清楚：**知道明文 SHA-256 的人可以拿到该对象的一个引用**。这不是新开的
    // 口子——跨用户秒传本身就允许任何登录用户探测「某份内容在不在系统里」，
    // 而摘要要么来自他自己持有的文件（那他本来就有内容），要么靠猜（SHA-256 猜不出来）。
    // 换来的是秒传真的生效。见 ATTACHMENT_ENCRYPTION_SPEC §6。
    let object = file_service
        .find_object_by_plaintext_sha256(&normalized)
        .await?
        // 🔴 与「服务端没有这份内容」是同一句话，不区分「没有」和「有但……」：
        // 分开说的话，这个接口就成了文件存在性探测器。
        .ok_or_else(|| ServerError::NotFound("服务端没有这份内容，请正常上传".to_string()))?;

    // 🔴 逻辑元数据全部取自**这次**的 token，一个字段都不从别人的记录复制。
    //
    // 复制过一版：第一个人上传 `离婚协议-张三李四.pdf`，第二个人按摘要秒传同一串
    // 字节之后，记录上写着第一个人的文件名。物理对象能提供的只有 object_id。
    let meta = crate::repository::file_upload_repo::ReferenceMetadata {
        original_filename: token.filename.as_deref().unwrap_or_default(),
        file_type: &token.file_type,
        mime_type: token.mime_type.as_deref().unwrap_or("application/octet-stream"),
        business_type: &token.business_type,
    };

    let file_id = file_service
        .create_reference(object.object_id, user_id, &meta, Some(&key))
        .await?;

    // 🔴 **不再消费 token。**
    //
    // 一次性消费原本是这条路径的防重放，但 token 现在最长 24 小时且可复用。
    // 幂等真正的承担者是 `uq_privchat_file_uploads_claim_key
    // (uploader_id, claim_key_hash)` 部分唯一索引——同一用户对同一份内容重复
    // claim 会收敛到同一行，而不是多出一行。
    let _ = token_service; // 保留参数签名，等清理批次一并移除

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
