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

//! **Legacy adapter**：V1 的裸 JSON metadata → 类型化 [`MediaRef`] + 审计
//! （spec `foundation/MEDIA_REFERENCE_AND_FORWARD_SPEC` §13）。
//!
//! 分层边界（冻结）：
//!
//! - **Protocol** 只表达类型化事实（`MessageMetadata::attachment_refs`），
//!   **不含**任何 migration / legacy 审计语义。
//! - **本层**负责 V1 兼容：content_type 映射、JSON 解析失败、字段缺失，
//!   并把这些如实记进 [`LegacyParseReport::audits`]。
//! - **消费策略**在调用方：在线写入用 strict（[`LegacyParseReport::into_strict`]），
//!   存量回填用 tolerant（直接读 `refs` + `audits`）。
//!
//! 🔴 曾经的错误做法：在服务层另写一份「在任意 JSON 上找 `file_id` /
//! `thumbnail_file_id`」的解析，并用 magic number 近似消息类型。那份解析
//! 认不出「类型决定字段」——文本消息伪造 `file_id` 也会产出引用；而协议层
//! 早就有类型化的那一份，`sync/submit` 一直在用。**不要再造第二份。**

use privchat_protocol::message::ContentMessageType;
use privchat_protocol::{MediaRef, MediaRole, MessageMetadata};
use serde_json::Value;

/// V1 兼容层在解析过程中发现的问题。回填必须**逐条记账**，不允许静默跳过——
/// 静默跳过会让「回填完成、零错误」变成谎话，真正缺引用的那批要等用户投诉才暴露。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LegacyAudit {
    /// content_type 声明是媒体类型，但 JSON 解不成对应的 typed variant
    /// （字段缺失 / 类型不符 / 历史格式）。
    MetadataUndecodable { content_type: i32 },
    /// 类型化解析成功，但一条引用都没有——媒体类型必须有主体文件。
    /// 常见于 legacy 行；回填归入隔离清单人工复核。
    NoRefsForMediaType { content_type: i32 },
    /// 解析出了引用，但缺少 Original（只有缩略图）。
    /// 在线写入必须拒绝；回填进隔离清单。
    MissingOriginal { content_type: i32 },
}

/// 一次 V1 解析的完整结果。**永远返回能解析出的部分**——
/// 回填要的是「尽可能补齐 + 问题清单」，不是「一条坏数据卡死整批」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyParseReport {
    pub refs: Vec<MediaRef>,
    pub audits: Vec<LegacyAudit>,
}

impl LegacyParseReport {
    /// 在线写入策略：**有任何审计问题就整条拒绝**。
    ///
    /// 与 tolerant 的区别只在这里——同一份解析结果，两种消费方式。
    /// 线上宁可拒绝一条可疑消息，也不能写进一份自己都不确定的引用。
    pub fn into_strict(self) -> Result<Vec<MediaRef>, LegacyAudit> {
        match self.audits.into_iter().next() {
            Some(audit) => Err(audit),
            None => Ok(self.refs),
        }
    }

    /// 去重后的 file_id，供按文件的操作（绑定守卫、引用计数）使用。
    pub fn unique_file_ids(&self) -> Vec<u64> {
        let mut ids: Vec<u64> = self.refs.iter().map(|r| r.file_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// 媒体类型：必须有主体文件的那些。位置/链接只有缩略图，不在此列。
fn requires_original(kind: ContentMessageType) -> bool {
    matches!(
        kind,
        ContentMessageType::Image
            | ContentMessageType::Video
            | ContentMessageType::Voice
            | ContentMessageType::File
    )
}

/// 从 V1 的 `(content_type, metadata JSON)` 解析出带角色的引用。
///
/// 走的是**协议层的类型化解析**：先 `from_json_value` 得到 typed variant，
/// 再 `attachment_refs()`。类型决定字段，所以文本/系统/资金消息即使 JSON 里
/// 塞了 `file_id` 也产不出引用。
pub fn parse_legacy_media_refs(kind: ContentMessageType, metadata: &Value) -> LegacyParseReport {
    parse_by_kind(kind, kind as i32, metadata)
}

/// 同上，但入参是数据库里的整数 content_type（回填 job 从消息表读到的形态）。
/// 未知数值不猜：既不产引用，也不算错误——新类型上线时老服务端会走到这里。
pub fn parse_legacy_media_refs_by_code(content_type: i32, metadata: &Value) -> LegacyParseReport {
    let Some(kind) = u32::try_from(content_type)
        .ok()
        .and_then(ContentMessageType::from_u32)
    else {
        // 未知类型：不猜。既不产引用，也不算错误——新类型上线时老服务端会走到这里。
        return LegacyParseReport {
            refs: Vec::new(),
            audits: Vec::new(),
        };
    };
    parse_by_kind(kind, content_type, metadata)
}

fn parse_by_kind(
    kind: ContentMessageType,
    content_type: i32,
    metadata: &Value,
) -> LegacyParseReport {
    let Some(typed) = MessageMetadata::from_json_value(kind, metadata) else {
        // 非媒体类型（Text / System / 资金）本来就没有 typed metadata，不是问题。
        let audits = if requires_original(kind) {
            vec![LegacyAudit::MetadataUndecodable { content_type }]
        } else {
            Vec::new()
        };
        return LegacyParseReport {
            refs: Vec::new(),
            audits,
        };
    };

    let refs = typed.attachment_refs();
    let mut audits = Vec::new();
    if requires_original(kind) {
        if refs.is_empty() {
            audits.push(LegacyAudit::NoRefsForMediaType { content_type });
        } else if !refs.iter().any(|r| r.role == MediaRole::Original) {
            audits.push(LegacyAudit::MissingOriginal { content_type });
        }
    }

    LegacyParseReport { refs, audits }
}

/// 已经持有 typed metadata 的写入路径（`sync/submit`、服务端注入）用这个投影。
///
/// 与 [`parse_legacy_media_refs`] 是**同一个协议层事实的两个入口**：一个从裸 JSON
/// 进，一个从 typed 进，出口都是 `MessageMetadata::unique_file_ids`。
/// 冻结点（spec §13）：绑定守卫只允许有这两个入口，不许再出现第三份解析。
pub fn typed_media_file_ids(metadata: Option<&MessageMetadata>) -> Vec<u64> {
    metadata.map(MessageMetadata::unique_file_ids).unwrap_or_default()
}

/// 三条写入路径共享的用例表（spec §12 门禁 1）。
///
/// 每条是 `(用例名, content_type, metadata JSON, 期望绑定的 file_id)`。
/// **任何一条写入路径的结果与这里不一致就是分叉**——分叉的后果是：
/// 同一张图片，正常发送能下载、`sync/submit` 补投的下不动。
#[cfg(test)]
pub(crate) fn shared_cases() -> Vec<(&'static str, ContentMessageType, Value, Vec<u64>)> {
    use serde_json::json;
    vec![
        (
            "图片：原图 + 缩略图",
            ContentMessageType::Image,
            json!({ "file_id": 100u64, "thumbnail_file_id": 200u64, "width": 800, "height": 600 }),
            vec![100, 200],
        ),
        (
            "图片：同一文件兼任两个角色 → 按文件算一个",
            ContentMessageType::Image,
            json!({ "file_id": 7u64, "thumbnail_file_id": 7u64 }),
            vec![7],
        ),
        (
            "图片：字符串形式 id（老客户端）",
            ContentMessageType::Image,
            json!({ "file_id": "300", "thumbnail_file_id": "0" }),
            vec![300],
        ),
        (
            "图片：雪花级 u64 无损",
            ContentMessageType::Image,
            json!({ "file_id": 9_007_199_254_740_993u64 }),
            vec![9_007_199_254_740_993],
        ),
        (
            "视频：原视频 + 缩略图",
            ContentMessageType::Video,
            json!({ "file_id": 11u64, "thumbnail_file_id": 12u64, "duration": 8 }),
            vec![11, 12],
        ),
        (
            "语音：只有主体文件",
            ContentMessageType::Voice,
            json!({ "file_id": 21u64, "duration": 3 }),
            vec![21],
        ),
        (
            "文件：只有主体文件",
            ContentMessageType::File,
            json!({ "file_id": 31u64, "file_name": "a.pdf", "file_size": 10 }),
            vec![31],
        ),
        (
            "文本伪造 file_id → 一个都不绑（类型决定字段）",
            ContentMessageType::Text,
            json!({ "text": "hi", "file_id": 100u64, "thumbnail_file_id": 200u64 }),
            vec![],
        ),
        (
            "资金消息的不透明 payload → 不绑",
            ContentMessageType::RedPacket,
            json!({ "file_id": 999u64 }),
            vec![],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEXT: i32 = ContentMessageType::Text as i32;
    const IMAGE: i32 = ContentMessageType::Image as i32;
    const VIDEO: i32 = ContentMessageType::Video as i32;
    const RED_PACKET: i32 = ContentMessageType::RedPacket as i32;

    /// 【发布门禁 2】非媒体消息伪带 `file_id` **不产生任何引用**。
    /// 这正是裸 JSON 解析做不到的——它只看字段名，不看类型。
    #[test]
    fn a_text_message_carrying_a_file_id_yields_nothing() {
        let report =
            parse_legacy_media_refs_by_code(TEXT, &json!({ "file_id": 999, "thumbnail_file_id": 998 }));
        assert!(report.refs.is_empty(), "文本消息不该产出引用");
        assert!(report.audits.is_empty(), "也不该报成错误——它本来就没附件");
    }

    /// 资金消息同理：payload 是不透明快照，服务端不解析成 typed metadata。
    #[test]
    fn a_money_message_carrying_a_file_id_yields_nothing() {
        let report = parse_legacy_media_refs_by_code(RED_PACKET, &json!({ "file_id": 999 }));
        assert!(report.refs.is_empty());
        assert!(report.audits.is_empty());
    }

    /// 【发布门禁 3】同一个 file 同时是 Original 和 Thumbnail，两条都保留。
    #[test]
    fn one_file_in_both_roles_keeps_two_refs_but_one_unique_id() {
        let report =
            parse_legacy_media_refs_by_code(IMAGE, &json!({ "file_id": 7, "thumbnail_file_id": 7 }));
        assert_eq!(report.refs.len(), 2);
        assert_eq!(report.refs[0].role, MediaRole::Original);
        assert_eq!(report.refs[1].role, MediaRole::Thumbnail);
        assert_eq!(report.unique_file_ids(), vec![7], "绑定守卫按文件算只有一个");
        assert!(report.audits.is_empty());
    }

    #[test]
    fn an_image_yields_the_original_and_the_thumbnail() {
        let report = parse_legacy_media_refs(
            ContentMessageType::Image,
            &json!({ "file_id": 22245, "thumbnail_file_id": 22244 }),
        );
        assert_eq!(
            report.refs.iter().map(|r| (r.file_id, r.role)).collect::<Vec<_>>(),
            vec![(22245, MediaRole::Original), (22244, MediaRole::Thumbnail)]
        );
        assert!(report.audits.is_empty());
    }

    /// 【发布门禁 4】只有缩略图、缺主体文件 → 在线拒绝、回填审计。
    /// 同一份解析结果，两种消费策略。
    #[test]
    fn a_thumbnail_without_an_original_is_refused_online_but_audited_for_backfill() {
        // 图片的 file_id 是必填字段，构造「只有缩略图」要用 video（file_id 缺省 0）。
        let report = parse_legacy_media_refs(
            ContentMessageType::Video,
            &json!({ "file_id": 0, "thumbnail_file_id": 22244, "duration": 3 }),
        );
        // tolerant：保留能确定的那条 + 记账
        assert_eq!(report.refs.len(), 1);
        assert_eq!(report.refs[0].role, MediaRole::Thumbnail);
        assert_eq!(
            report.audits,
            vec![LegacyAudit::MissingOriginal { content_type: VIDEO }]
        );
        // strict：整条拒绝
        assert!(report.clone().into_strict().is_err());
    }

    #[test]
    fn a_media_message_whose_metadata_cannot_be_decoded_is_audited() {
        let report = parse_legacy_media_refs_by_code(IMAGE, &json!({ "not": "an image" }));
        assert!(report.refs.is_empty());
        assert_eq!(
            report.audits,
            vec![LegacyAudit::MetadataUndecodable { content_type: IMAGE }]
        );
        assert!(report.into_strict().is_err());
    }

    /// 未知 content_type（新类型 / 老服务端）：不猜，也不报错。
    #[test]
    fn an_unknown_content_type_is_neither_parsed_nor_reported() {
        let report = parse_legacy_media_refs_by_code(9999, &json!({ "file_id": 1 }));
        assert!(report.refs.is_empty());
        assert!(report.audits.is_empty());
    }

    /// 【发布门禁 5】超大 u64 无损。TS 的 number 只有 2^53，
    /// 雪花 ID 走 JSON 数字会被舍入成另一个文件。
    #[test]
    fn a_snowflake_sized_file_id_survives_intact() {
        let big: u64 = 18_446_744_073_709_551_615; // u64::MAX
        let report = parse_legacy_media_refs_by_code(IMAGE, &json!({ "file_id": big }));
        assert_eq!(report.refs[0].file_id, big);
    }

    /// 干净的解析在 strict 下原样通过。
    #[test]
    fn a_clean_parse_passes_strict_unchanged() {
        let report = parse_legacy_media_refs_by_code(IMAGE, &json!({ "file_id": 1, "thumbnail_file_id": 2 }));
        let refs = report.clone().into_strict().expect("应当通过");
        assert_eq!(refs, report.refs);
    }

    /// 【发布门禁 1】两个入口（裸 JSON / typed）在同一组用例上必须给出同一结果。
    /// 分叉的现实后果：同一张图正常发送能下载，`sync/submit` 补投的下不动。
    #[test]
    fn both_entry_points_agree_on_every_shared_case() {
        for (name, kind, meta, expected) in shared_cases() {
            let from_json = parse_legacy_media_refs(kind, &meta).unique_file_ids();
            let from_typed =
                typed_media_file_ids(MessageMetadata::from_json_value(kind, &meta).as_ref());
            assert_eq!(from_json, expected, "裸 JSON 入口结果不符：{name}");
            assert_eq!(from_typed, expected, "typed 入口结果不符：{name}");
        }
    }
}
