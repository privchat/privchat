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

use crate::repository::message_repo::{AttachmentOrigin, ForwardOrigin};

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

/// 目标消息的来源快照（§6.2）：转发一条转发消息时，root 沿用源消息的 root，
/// **不是上一手**——展示「转发自最初作者」，与微信/Telegram 一致。
pub fn root_origin_for_copy(
    source_message_id: u64,
    source_channel_id: u64,
    source_author_id: u64,
    source_origin: Option<ForwardOrigin>,
) -> ForwardOrigin {
    match source_origin {
        Some(existing) => existing,
        None => ForwardOrigin {
            root_message_id: Some(source_message_id),
            root_author_id: source_author_id,
            root_channel_id: Some(source_channel_id),
            display_snapshot: None,
        },
    }
}

/// 复制到目标消息的媒体引用。
///
/// 优先用源消息的**引用表**行（权威）；引用表为空时（存量消息、回填尚未覆盖）
/// 退回按源 metadata 解析。两条路径都走同一个 canonical parser。
pub fn refs_for_copy(
    refs_from_table: Vec<MediaRef>,
    source_message_type: i32,
    source_metadata: &serde_json::Value,
) -> Vec<MediaRef> {
    if !refs_from_table.is_empty() {
        return refs_from_table;
    }
    crate::service::legacy_media_refs::parse_legacy_media_refs_by_code(
        source_message_type,
        source_metadata,
    )
    .refs
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

    /// 转发一条转发消息：root 沿用源消息的 root，不是上一手。
    #[test]
    fn forwarding_a_forward_keeps_the_original_author() {
        let source_origin = ForwardOrigin {
            root_message_id: Some(11),
            root_author_id: 111,
            root_channel_id: Some(1111),
            display_snapshot: None,
        };
        let copied = root_origin_for_copy(22, 2222, 222, Some(source_origin));
        assert_eq!(copied.root_message_id, Some(11));
        assert_eq!(copied.root_author_id, 111, "展示的是最初作者，不是上一手转发人");
    }

    /// 转发一条原创消息：源消息自己就是 root。
    #[test]
    fn forwarding_an_original_makes_it_the_root() {
        let copied = root_origin_for_copy(22, 2222, 222, None);
        assert_eq!(copied.root_message_id, Some(22));
        assert_eq!(copied.root_author_id, 222);
        assert_eq!(copied.root_channel_id, Some(2222));
    }

    /// 引用表有行就用它；没有才回落解析 metadata（存量消息）。
    #[test]
    fn refs_come_from_the_table_first_and_the_metadata_only_as_a_fallback() {
        use privchat_protocol::MediaRole;
        let from_table = vec![MediaRef {
            file_id: 7,
            role: MediaRole::Original,
            ordinal: 0,
        }];
        let metadata = serde_json::json!({ "file_id": 99, "thumbnail_file_id": 98 });
        assert_eq!(
            refs_for_copy(from_table.clone(), ContentMessageType::Image as i32, &metadata),
            from_table,
            "引用表是权威，不该被 metadata 覆盖",
        );

        let fallback = refs_for_copy(Vec::new(), ContentMessageType::Image as i32, &metadata);
        assert_eq!(
            fallback.iter().map(|r| r.file_id).collect::<Vec<_>>(),
            vec![99, 98],
            "存量消息（回填未覆盖）回落解析 metadata",
        );
    }

    /// 文本消息伪造 file_id，转发时也不会凭空复制出附件引用。
    #[test]
    fn a_forwarded_text_message_copies_no_attachments() {
        let metadata = serde_json::json!({ "text": "hi", "file_id": 5 });
        assert!(
            refs_for_copy(Vec::new(), ContentMessageType::Text as i32, &metadata).is_empty()
        );
    }
}
