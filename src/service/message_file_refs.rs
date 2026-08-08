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

//! 消息 → 附件引用的**唯一解析口**（spec `foundation/MEDIA_REFERENCE_AND_FORWARD_SPEC` §9.2）。
//!
//! 发送、转发、以及存量回填 job **必须**调用这里的同一个函数。曾经的实现是
//! `SendMessageHandler` 的一个私有方法，只返回 `Vec<u64>`——没有 role，也没有
//! ordinal，填不满引用表；而回填如果另写一份 JSON 路径解析，两份实现必然漂移，
//! 那正是「回填全绿但线上仍然失败」的经典成因。
//!
//! 🔴 **禁止**在 migration SQL 里手写第二份解析。回填以独立的 Rust job 交付。

use serde_json::Value;

/// 一个文件在消息里扮演的角色。数值即入库的 `message_file_refs.role`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(i16)]
pub enum FileRefRole {
    /// 主体文件（图片原图 / 视频 / 语音 / 普通文件）。
    Original = 0,
    /// 缩略图。**独立的 file_id 与 CEK**，不是主体文件的附属物——
    /// 接收端要单独走一次 `file/get_url` 才能解密（ATTACHMENT_ENCRYPTION_SPEC §6.2）。
    Thumbnail = 1,
    /// 预留：预览图 / 转码版本。v1 不产出。
    Preview = 2,
}

impl FileRefRole {
    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

/// 一条消息引用的一个文件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageFileRef {
    pub file_id: u64,
    pub role: FileRefRole,
    /// 同一 role 下的序号。v1 每种 role 至多一个，恒为 0；
    /// 多图消息（一条消息 N 张图）落地时才会 > 0。
    pub ordinal: i32,
}

/// 解析过程中发现的**数据问题**。
///
/// 回填时必须把这些**逐条记录进审计报告**，不允许静默跳过——静默跳过会让
/// 「回填完成、零错误」这句话变成谎话，而真正缺引用的那批消息要等用户投诉才暴露。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRefAudit {
    /// 字段存在但解析不出正整数（负数 / 0 / 非数字字符串 / 浮点等）。
    Invalid {
        key: &'static str,
        raw: String,
    },
    /// 消息类型按约定应当带附件，metadata 里却一个都没有。
    /// 常见于 legacy 行；回填时归入 `missing` 桶人工复核。
    MissingForType {
        message_type: i32,
    },
    /// 同一个 file_id 在多个 role 上重复出现（例如 file_id == thumbnail_file_id）。
    /// 取先出现的 role，另一个丢弃并记账。
    DuplicateFileId {
        file_id: u64,
        kept: FileRefRole,
        dropped: FileRefRole,
    },
}

/// 需要带附件的消息类型（`ContentMessageType` 数值）。
///
/// 只用于「该有却没有」的审计判断，**不参与授权**。授权一律走引用存在性（spec §4.1）。
const TYPES_EXPECTING_ATTACHMENT: &[i32] = &[
    1, // Voice
    2, // Image
    3, // Video
    4, // File
];

/// 从消息 metadata 解析出全部附件引用。
///
/// 返回 `(refs, audits)`：**永远返回能解析出来的部分**，不因为个别字段有问题就整条失败——
/// 回填要的是「尽可能补齐 + 问题清单」，而不是「一条坏数据卡死整批」。
///
/// 覆盖 image/video/voice/file 的 `file_id`，以及 video/link/location 的 `thumbnail_file_id`。
/// 数字与字符串两种写法都认（历史上两种都发过）。
pub fn extract_message_file_refs(
    message_type: i32,
    metadata: &Value,
) -> (Vec<MessageFileRef>, Vec<FileRefAudit>) {
    let mut refs: Vec<MessageFileRef> = Vec::new();
    let mut audits: Vec<FileRefAudit> = Vec::new();

    for (key, role) in [
        ("file_id", FileRefRole::Original),
        ("thumbnail_file_id", FileRefRole::Thumbnail),
    ] {
        let Some(raw) = metadata.get(key) else {
            continue;
        };
        // 显式 null 等同于「没写这个字段」，不是坏数据。
        if raw.is_null() {
            continue;
        }
        match parse_file_id(raw) {
            Some(file_id) => {
                if let Some(existing) = refs.iter().find(|r| r.file_id == file_id) {
                    audits.push(FileRefAudit::DuplicateFileId {
                        file_id,
                        kept: existing.role,
                        dropped: role,
                    });
                    continue;
                }
                refs.push(MessageFileRef {
                    file_id,
                    role,
                    ordinal: 0,
                });
            }
            None => audits.push(FileRefAudit::Invalid {
                key,
                raw: raw.to_string(),
            }),
        }
    }

    if refs.is_empty() && TYPES_EXPECTING_ATTACHMENT.contains(&message_type) {
        audits.push(FileRefAudit::MissingForType { message_type });
    }

    (refs, audits)
}

/// 只要 file_id 列表（保持与旧 `extract_attachment_file_ids` 完全一致的行为），
/// 供发送路径的绑定守卫使用。顺序 = Original 先于 Thumbnail。
pub fn extract_attachment_file_ids(metadata: &Value) -> Vec<u64> {
    extract_message_file_refs(-1, metadata)
        .0
        .into_iter()
        .map(|r| r.file_id)
        .collect()
}

/// 数字或字符串均可；0、负数、浮点、非数字一律视为无效。
fn parse_file_id(v: &Value) -> Option<u64> {
    let id = v
        .as_u64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse::<u64>().ok()))?;
    (id > 0).then_some(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_image_yields_the_payload_and_the_thumbnail_as_separate_roles() {
        let (refs, audits) = extract_message_file_refs(
            2,
            &json!({ "file_id": 22245, "thumbnail_file_id": 22244 }),
        );
        assert_eq!(
            refs,
            vec![
                MessageFileRef { file_id: 22245, role: FileRefRole::Original, ordinal: 0 },
                MessageFileRef { file_id: 22244, role: FileRefRole::Thumbnail, ordinal: 0 },
            ]
        );
        assert!(audits.is_empty());
    }

    /// 历史上两种写法都发过：数字和十进制字符串。两种都必须认，
    /// 否则回填会把一半的存量消息判成「没有附件」。
    #[test]
    fn a_file_id_written_as_a_string_counts_the_same_as_a_number() {
        let (from_num, _) = extract_message_file_refs(2, &json!({ "file_id": 22245 }));
        let (from_str, _) = extract_message_file_refs(2, &json!({ "file_id": "22245" }));
        assert_eq!(from_num, from_str);
    }

    #[test]
    fn a_text_message_has_no_refs_and_nothing_to_report() {
        let (refs, audits) = extract_message_file_refs(0, &json!({ "text": "hi" }));
        assert!(refs.is_empty());
        assert!(audits.is_empty(), "文本消息本来就没附件，不该报 missing");
    }

    /// 「该有附件却一个都没有」必须进审计桶，不能静默当成空结果——
    /// 那正是回填漏引用后无人察觉的路径。
    #[test]
    fn an_image_without_any_file_id_is_reported_not_silently_skipped() {
        let (refs, audits) = extract_message_file_refs(2, &json!({ "text": "" }));
        assert!(refs.is_empty());
        assert_eq!(audits, vec![FileRefAudit::MissingForType { message_type: 2 }]);
    }

    #[test]
    fn a_malformed_file_id_is_reported_and_the_rest_still_parses() {
        let (refs, audits) = extract_message_file_refs(
            3,
            &json!({ "file_id": "not-a-number", "thumbnail_file_id": 22244 }),
        );
        assert_eq!(
            refs,
            vec![MessageFileRef { file_id: 22244, role: FileRefRole::Thumbnail, ordinal: 0 }],
            "坏字段不该拖垮好字段"
        );
        assert_eq!(
            audits,
            vec![FileRefAudit::Invalid { key: "file_id", raw: "\"not-a-number\"".to_string() }]
        );
    }

    #[test]
    fn zero_and_negative_ids_are_invalid_not_valid_references() {
        for bad in [json!(0), json!(-1), json!("0")] {
            let (refs, audits) = extract_message_file_refs(4, &json!({ "file_id": bad }));
            assert!(refs.is_empty(), "{bad} 不该被当成有效 file_id");
            assert!(audits.iter().any(|a| matches!(a, FileRefAudit::Invalid { .. })));
        }
    }

    /// null 是「没写这个字段」，不是坏数据——报成 Invalid 会让审计报告里
    /// 塞满噪音，真正的问题反而被淹没。
    #[test]
    fn an_explicit_null_is_absence_not_corruption() {
        let (refs, audits) =
            extract_message_file_refs(2, &json!({ "file_id": 1, "thumbnail_file_id": null }));
        assert_eq!(refs.len(), 1);
        assert!(audits.is_empty());
    }

    /// 同一个 id 出现在两个 role 上，入库会撞主键之外的语义（一个文件两种身份）。
    /// 保留先出现的，另一个记账丢弃。
    #[test]
    fn the_same_id_in_two_roles_keeps_one_and_records_the_drop() {
        let (refs, audits) = extract_message_file_refs(
            2,
            &json!({ "file_id": 999, "thumbnail_file_id": 999 }),
        );
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].role, FileRefRole::Original);
        assert_eq!(
            audits,
            vec![FileRefAudit::DuplicateFileId {
                file_id: 999,
                kept: FileRefRole::Original,
                dropped: FileRefRole::Thumbnail,
            }]
        );
    }

    /// 与旧 `SendMessageHandler::extract_attachment_file_ids` 行为等价：
    /// 发送路径的绑定守卫依赖这个顺序与去重语义，换实现不能改变它。
    #[test]
    fn the_id_only_view_matches_the_legacy_behaviour() {
        let meta = json!({ "file_id": 10, "thumbnail_file_id": 11 });
        assert_eq!(extract_attachment_file_ids(&meta), vec![10, 11]);
        assert!(extract_attachment_file_ids(&json!({})).is_empty());
        assert_eq!(
            extract_attachment_file_ids(&json!({ "thumbnail_file_id": 7 })),
            vec![7]
        );
    }

    /// role 数值是入库值，改动会让存量行与新行语义不一致。
    #[test]
    fn role_wire_values_are_frozen() {
        assert_eq!(FileRefRole::Original.as_i16(), 0);
        assert_eq!(FileRefRole::Thumbnail.as_i16(), 1);
        assert_eq!(FileRefRole::Preview.as_i16(), 2);
    }
}
