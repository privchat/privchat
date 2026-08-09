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

//! 单条转发（MEDIA_REFERENCE_AND_FORWARD_SPEC §6）。
//!
//! 转发产生的是**目标会话里的一条独立消息**（快照），不是指向源消息的链接。
//! 源消息之后被撤回、删除、甚至物理清除，都不追溯影响这条副本（§0 核心不变量）。
//!
//! 🔴 客户端只说「转发哪条、转到哪」。正文、媒体引用全部由服务端从源消息复制，
//! 所以「伪造媒体描述符」在构造上就不存在，不需要 HMAC capability 那一套。

use privchat_protocol::message::ContentMessageType;
use privchat_protocol::MediaRef;
use std::collections::BTreeSet;

use crate::repository::message_repo::AttachmentOrigin;

/// 转发被拒的原因。每一条都对应 spec 里一条明确规则，不用笼统的「失败」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForwardRefusal {
    /// 源消息不存在。
    SourceNotFound,
    /// 源消息已删除或已撤回（§6.4：不能绕过消息凭 file_id 转发）。
    SourceGone,
    /// 转发人读不到源消息。
    SourceNotReadable,
    /// 这个类型不允许转发（§6.1 白名单）。
    TypeNotAllowed(ContentMessageType),
    /// 转发人不在目标会话里。
    TargetNotWritable,
    /// 客户端声称的 source_channel_id 与源消息真实所在会话不符。
    SourceChannelMismatch,
    /// 源会话开启了内容保护（禁止转发）。对齐 Telegram CHAT_FORWARDS_RESTRICTED。
    ForwardsRestricted,
    /// 源消息在「读出来」与「写下去」之间被改过（内容或引用）。
    SourceChanged,
    /// 源消息的媒体引用不完整（存量破损数据）。转发会把坏账复制成新消息。
    SourceMediaIncomplete(crate::service::legacy_media_refs::LegacyAudit),
    /// 引用表与 metadata 互相矛盾。复制过去会产出一条「客户端按 metadata 渲染、
    /// 服务端按引用表授权」两边指向不同文件的消息。
    SourceMediaInconsistent {
        in_reference_table: Vec<(u64, i16, i32)>,
        in_metadata: Vec<(u64, i16, i32)>,
    },
}

impl ForwardRefusal {
    /// 稳定的错误标识，客户端据此做文案与重试决策。
    pub fn code(&self) -> &'static str {
        match self {
            ForwardRefusal::SourceNotFound => "FORWARD_SOURCE_NOT_FOUND",
            ForwardRefusal::SourceGone => "FORWARD_SOURCE_GONE",
            ForwardRefusal::SourceNotReadable => "FORWARD_SOURCE_NOT_READABLE",
            ForwardRefusal::TypeNotAllowed(_) => "FORWARD_TYPE_NOT_ALLOWED",
            ForwardRefusal::TargetNotWritable => "FORWARD_TARGET_NOT_WRITABLE",
            ForwardRefusal::SourceChannelMismatch => "FORWARD_SOURCE_CHANNEL_MISMATCH",
            ForwardRefusal::ForwardsRestricted => "FORWARDS_RESTRICTED",
            ForwardRefusal::SourceChanged => "FORWARD_SOURCE_CHANGED",
            ForwardRefusal::SourceMediaIncomplete(_) => "FORWARD_SOURCE_MEDIA_INCOMPLETE",
            ForwardRefusal::SourceMediaInconsistent { .. } => "FORWARD_SOURCE_MEDIA_INCONSISTENT",
        }
    }
}

/// 可转发类型白名单（§6.1，冻结）。
///
/// 🔴 **白名单制**：没列在这里的类型一律拒绝。新增消息类型时必须显式评估——
/// 默认允许的话，下一个「服务端注入的卡片」类型会在没人注意时变成可转发。
///
/// 资金类（红包 / 转账）永远禁止：卡片是服务端按真实订单注入的产物，
/// 复制一份等于造出一张没有订单支撑的卡。这条防线原本在 TS 客户端里，
/// 服务端 RPC 必须接过来——否则新 RPC 反而把旧防线拆了。
pub fn is_forwardable(message_type: ContentMessageType) -> bool {
    matches!(
        message_type,
        ContentMessageType::Text
            | ContentMessageType::Image
            | ContentMessageType::Video
            | ContentMessageType::Voice
            | ContentMessageType::File
            | ContentMessageType::Location
            | ContentMessageType::Link
    )
}

/// 复制到目标消息的媒体引用。
///
/// 🔴 **永远 strict 解析 metadata，然后要求引用表与之完全一致。**
///
/// 我第一版写成「引用表非空就直接采用」，于是 strict 校验被整条路径绕过：
/// 引用表说 file 7、metadata 说 file 99/98，照样转发成功，产出一条
/// **metadata 与授权引用互相矛盾**的消息——客户端按 metadata 渲染缩略图，
/// 服务端按引用表授权下载，两边指向不同的文件，就是一张永远修不好的裂图。
/// 更糟的是当时的测试把这个行为写成了断言，等于给错误盖章。
///
/// 规则：
/// ```text
/// 永远 strict 解析 metadata
/// 引用表为空   → 用解析结果（存量消息，回填尚未覆盖）
/// 引用表非空   → 必须与解析结果**集合相等**，否则拒绝
/// ```
pub fn refs_for_copy(
    refs_from_table: Vec<MediaRef>,
    source_message_type: i32,
    source_metadata: &serde_json::Value,
) -> Result<Vec<MediaRef>, ForwardRefusal> {
    let parsed = crate::service::legacy_media_refs::parse_legacy_media_refs_by_code(
        source_message_type,
        source_metadata,
    )
    .into_strict()
    .map_err(ForwardRefusal::SourceMediaIncomplete)?;

    if refs_from_table.is_empty() {
        return Ok(parsed);
    }

    let as_set = |refs: &[MediaRef]| -> BTreeSet<(u64, i16, i32)> {
        refs.iter()
            .map(|r| (r.file_id, r.role as i16, r.ordinal))
            .collect()
    };
    if as_set(&refs_from_table) != as_set(&parsed) {
        return Err(ForwardRefusal::SourceMediaInconsistent {
            in_reference_table: as_set(&refs_from_table).into_iter().collect(),
            in_metadata: as_set(&parsed).into_iter().collect(),
        });
    }
    Ok(refs_from_table)
}

/// 转发路径给提交请求用的附件来源。抽成常量是为了让「转发必须跳过归属守卫」
/// 这件事有个可搜索的名字，而不是散在调用点的一个枚举值。
pub const FORWARD_ATTACHMENT_ORIGIN: AttachmentOrigin = AttachmentOrigin::CopiedFromExistingMessage;

#[cfg(test)]
mod tests {
    use super::*;

    /// 【spec §6.1 / 验收 12】资金卡片不可转发，服务端拦住，不依赖客户端自觉。
    #[test]
    fn money_messages_are_never_forwardable() {
        assert!(!is_forwardable(ContentMessageType::RedPacket));
        assert!(!is_forwardable(ContentMessageType::MoneyTransfer));
    }

    /// 系统消息 / 服务指令同理。
    #[test]
    fn system_messages_are_never_forwardable() {
        assert!(!is_forwardable(ContentMessageType::System));
    }

    /// 白名单里的类型都放行。
    #[test]
    fn ordinary_content_is_forwardable() {
        for kind in [
            ContentMessageType::Text,
            ContentMessageType::Image,
            ContentMessageType::Video,
            ContentMessageType::Voice,
            ContentMessageType::File,
            ContentMessageType::Location,
            ContentMessageType::Link,
        ] {
            assert!(is_forwardable(kind), "{kind:?} 应当可转发");
        }
    }

    /// 🔴 白名单制的守卫：这条测试会在**新增消息类型**时提醒作者显式评估。
    /// 如果新类型被默认允许，这里会红。
    #[test]
    fn the_allow_list_is_exactly_these_seven_types() {
        let allowed: Vec<ContentMessageType> = [
            ContentMessageType::Text,
            ContentMessageType::Voice,
            ContentMessageType::Image,
            ContentMessageType::Video,
            ContentMessageType::File,
            ContentMessageType::System,
            ContentMessageType::Sticker,
            ContentMessageType::ContactCard,
            ContentMessageType::Location,
            ContentMessageType::Link,
            ContentMessageType::Forward,
            ContentMessageType::RedPacket,
            ContentMessageType::MoneyTransfer,
        ]
        .into_iter()
        .filter(|kind| is_forwardable(*kind))
        .collect();
        assert_eq!(allowed.len(), 7, "白名单变了就必须显式改这条断言：{allowed:?}");
    }

    /// 【P0】源消息媒体破损 → 拒绝转发，不复制出一条新的裂图消息。
    #[test]
    fn a_source_with_broken_media_is_refused_instead_of_copied() {
        // 图片消息但 metadata 解不出 typed variant（历史坏账）
        let undecodable = serde_json::json!({ "not": "an image" });
        let refusal = refs_for_copy(Vec::new(), ContentMessageType::Image as i32, &undecodable)
            .expect_err("破损媒体必须拒绝");
        assert_eq!(refusal.code(), "FORWARD_SOURCE_MEDIA_INCOMPLETE");

        // 只有缩略图、缺原图
        let thumb_only =
            serde_json::json!({ "file_id": 0, "thumbnail_file_id": 22244, "duration": 3 });
        assert!(
            refs_for_copy(Vec::new(), ContentMessageType::Video as i32, &thumb_only).is_err(),
            "缺主体文件的媒体不能被转发成新消息",
        );
    }

    /// 每个拒绝理由都有稳定标识：客户端按它决定文案与是否可重试。
    #[test]
    fn every_refusal_has_a_stable_code() {
        let codes = [
            ForwardRefusal::SourceNotFound.code(),
            ForwardRefusal::SourceGone.code(),
            ForwardRefusal::SourceNotReadable.code(),
            ForwardRefusal::TargetNotWritable.code(),
            ForwardRefusal::SourceChannelMismatch.code(),
            ForwardRefusal::ForwardsRestricted.code(),
            ForwardRefusal::SourceChanged.code(),
        ];
        let unique: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(unique.len(), codes.len(), "拒绝码不能重复：{codes:?}");
        assert!(
            codes.contains(&"FORWARDS_RESTRICTED"),
            "§6.3 内容保护必须有自己的码，对齐 Telegram CHAT_FORWARDS_RESTRICTED",
        );
    }



    /// 引用表与 metadata 必须一致；一致时采用，冲突时拒绝。
    ///
    /// 🔴 这条测试之前写反了：它断言「引用表 file 7 / metadata file 99、98」
    /// 可以通过，等于给「产出一条自相矛盾的消息」盖了章。
    #[test]
    fn the_reference_table_and_the_metadata_must_agree() {
        use privchat_protocol::MediaRole;
        let metadata = serde_json::json!({ "file_id": 99, "thumbnail_file_id": 98 });
        let consistent = vec![
            MediaRef {
                file_id: 99,
                role: MediaRole::Original,
                ordinal: 0,
            },
            MediaRef {
                file_id: 98,
                role: MediaRole::Thumbnail,
                ordinal: 0,
            },
        ];
        assert_eq!(
            refs_for_copy(
                consistent.clone(),
                ContentMessageType::Image as i32,
                &metadata
            )
            .expect("两边一致时采用引用表"),
            consistent,
        );

        let conflicting = vec![MediaRef {
            file_id: 7,
            role: MediaRole::Original,
            ordinal: 0,
        }];
        let refusal = refs_for_copy(conflicting, ContentMessageType::Image as i32, &metadata)
            .expect_err("引用表与 metadata 冲突必须拒绝");
        assert_eq!(refusal.code(), "FORWARD_SOURCE_MEDIA_INCONSISTENT");

        let fallback = refs_for_copy(Vec::new(), ContentMessageType::Image as i32, &metadata)
            .expect("引用表为空时回落解析 metadata");
        assert_eq!(
            fallback.iter().map(|r| r.file_id).collect::<Vec<_>>(),
            vec![99, 98],
        );
    }

    /// 引用表非空**也要**过 strict：破损 metadata 不能靠「表里有行」绕过去。
    #[test]
    fn a_non_empty_reference_table_does_not_bypass_strict_parsing() {
        use privchat_protocol::MediaRole;
        let undecodable = serde_json::json!({ "not": "an image" });
        let refs = vec![MediaRef {
            file_id: 7,
            role: MediaRole::Original,
            ordinal: 0,
        }];
        assert_eq!(
            refs_for_copy(refs, ContentMessageType::Image as i32, &undecodable)
                .expect_err("metadata 解不出就该拒绝，哪怕引用表有行")
                .code(),
            "FORWARD_SOURCE_MEDIA_INCOMPLETE",
        );
    }

    /// 文本消息伪造 file_id，转发时也不会凭空复制出附件引用。
    #[test]
    fn a_forwarded_text_message_copies_no_attachments() {
        let metadata = serde_json::json!({ "text": "hi", "file_id": 5 });
        assert!(refs_for_copy(Vec::new(), ContentMessageType::Text as i32, &metadata)
            .expect("文本消息不是媒体类型，没有审计问题")
            .is_empty());
    }
}
