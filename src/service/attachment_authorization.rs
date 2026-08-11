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

//! 附件访问授权的**唯一**判定入口（MEDIA_REFERENCE_AND_FORWARD_SPEC §4.1）。
//!
//! 🔴 这一层存在的理由：`file/get_url` 的判定曾经有两份——RPC 里一份，
//! DB 集成测试里又照抄一份。于是「测试全绿」只证明了那份抄件自洽，
//! 真正改了 RPC 反而是测试先炸。判定只能有一份，测试必须调它。
//!
//! 判据（spec §4.1）：
//!
//! ```text
//! 放行 ⟺ 存在一条引用该文件的消息 M，且
//!          M.deleted = false AND M.revoked = false
//!          AND requester 是 M.channel_id 的成员
//!      或  文件从未被任何消息引用（pending）AND requester == uploader
//! ```

use crate::model::file_upload::FileMetadata;
use crate::repository::PgMessageRepository;
use crate::service::file_service::{authorize_file_access, FileAccessFacts};
use crate::service::ChannelService;


/// 候选消息的来源。`Legacy` 归零才代表回填补齐（spec §10.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateSource {
    /// 引用表（权威）。
    ReferenceTable,
    /// 老的 `business_id` 单点绑定（过渡期兜底）。
    ///
    /// 🔴 只有**真的走了这条路**才算：文件确实带 `business_id`。
    /// 把「引用表为空」一律记成 legacy，会让正常的 pending 上传也计进来，
    /// 于是这个指标永远归不了零，而它的唯一用途就是「归零 = 回填补齐」。
    LegacyBusinessId,
    /// 文件还没被任何消息引用，也没有 `business_id`——正常的待发送上传。
    PendingUpload,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachmentAccessDecision {
    pub authorized: bool,
    pub source: CandidateSource,
    /// 找到了多少条候选引用（含已失效的）。
    pub candidate_count: usize,
}

/// 解析一个文件对某个请求者的访问权。
///
/// 两条**发现候选消息**的路径，一套判据：先查引用表，空了才按 `business_id` 找。
/// 🔴 兜底路径不是回落到旧语义——它同样过 `deleted` / `revoked` 过滤，
/// 否则 §4.2 那个「撤回后附件仍可下载」的洞会从这个入口原样回来。
pub async fn resolve_attachment_access(
    message_repository: &PgMessageRepository,
    channel_service: &ChannelService,
    file_meta: &FileMetadata,
    requester_id: u64,
) -> Result<AttachmentAccessDecision, AttachmentAccessError> {
    // 🔴 fail closed：查询失败**必须**变成拒绝，不能 `unwrap_or_default()`。
    // 吞成空列表的后果不是「少放行一次」，而是文件被当成从未被引用（pending），
    // 于是回落到「uploader 可读」——数据库一抖动，授权就松一档。
    let mut candidates = message_repository
        .file_reference_channels(file_meta.file_id)
        .await
        .map_err(|error| AttachmentAccessError::Unavailable(error.to_string()))?;

    let mut source = CandidateSource::ReferenceTable;
    // 「有过绑定但解析不出消息」与「从未绑定过」必须分开。前者是 broken binding：
    // 文件曾经属于某条消息，那条消息已经不在了 —— 这时**连上传者也不放行**，
    // 因为放行等于「消息被硬删后，附件反而回到上传者手里」。
    // 后者才是 pending，回落 uploader。
    let mut has_broken_legacy_binding = false;

    if candidates.is_empty() {
        let legacy_message_id = file_meta
            .business_id
            .as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|id| *id > 0);
        source = match legacy_message_id {
            Some(_) => CandidateSource::LegacyBusinessId,
            None => CandidateSource::PendingUpload,
        };
        if let Some(message_id) = legacy_message_id {
            match message_repository.live_channel_of_message(message_id).await {
                Ok(Some(entry)) => candidates.push(entry),
                // 消息不存在：broken binding，不猜、不放行。
                Ok(None) => has_broken_legacy_binding = true,
                // 查询失败是「不知道」，不是「不存在」——同样不能放行。
                Err(error) => {
                    return Err(AttachmentAccessError::Unavailable(error.to_string()));
                }
            }
        }
    }

    // 只问一次「这些有效引用所在的会话里，我是不是任一成员」——存在性判定，
    // 命中即止。逐个查在热门文件被转发到大量会话后是 N+1；批量入口做在
    // ChannelService 里，两边共用同一份成员规则，不在授权路径复制一份成员 SQL。
    let live_channels: Vec<u64> = candidates
        .iter()
        .filter(|(_, live)| *live)
        .map(|(channel_id, _)| *channel_id)
        .collect();
    // 🔴 直查库，不走 ChannelService。它的成员读取命中内存缓存就返回，
    // 缓存陈旧多久，已经退群的人就还能读多久附件。授权不能建立在缓存上。
    //
    // `channel_service` 仍在签名里：调用方拿它做别的事，而且这里换判据是一次
    // 收敛，不想顺手改所有调用点的形状。
    let _ = channel_service;
    let requester_is_member_of_a_live_reference = message_repository
        .is_member_of_any_channel(&live_channels, requester_id)
        .await
        .map_err(|error| AttachmentAccessError::Unavailable(error.to_string()))?;

    let authorized = authorize_file_access(FileAccessFacts {
        requester_id,
        uploader_id: file_meta.uploader_id,
        has_any_reference: !candidates.is_empty() || has_broken_legacy_binding,
        requester_is_member_of_a_live_reference,
    });

    Ok(AttachmentAccessDecision {
        authorized,
        source,
        candidate_count: candidates.len(),
    })
}

/// 判定不出来（数据库故障等）。**不是拒绝**，也**不是放行**——
/// 调用方必须把它映射成 5xx / 内部错误，让客户端知道这是可重试的服务异常。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentAccessError {
    Unavailable(String),
}

impl std::fmt::Display for AttachmentAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AttachmentAccessError::Unavailable(detail) => {
                write!(f, "附件授权判定不可用: {detail}")
            }
        }
    }
}
