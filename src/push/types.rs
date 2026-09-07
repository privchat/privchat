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

use serde::{Deserialize, Serialize};

/// 推送平台
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PushVendor {
    Apns,
    Fcm,
    Hms,    // Huawei / HarmonyOS push
    Xiaomi, // Mi Push
    Oppo,   // HeyTap Push
    Vivo,   // Vivo Push
    Honor,  // Honor Push (通常与 HMS 生态兼容)
    Lenovo, // Lenovo Push
    Zte,    // ZTE Push
    Meizu,  // Meizu Push
}

impl PushVendor {
    pub fn as_str(&self) -> &'static str {
        match self {
            PushVendor::Apns => "apns",
            PushVendor::Fcm => "fcm",
            PushVendor::Hms => "hms",
            PushVendor::Xiaomi => "xiaomi",
            PushVendor::Oppo => "oppo",
            PushVendor::Vivo => "vivo",
            PushVendor::Honor => "honor",
            PushVendor::Lenovo => "lenovo",
            PushVendor::Zte => "zte",
            PushVendor::Meizu => "meizu",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "apns" => Some(PushVendor::Apns),
            "fcm" => Some(PushVendor::Fcm),
            "hms" | "huawei" | "huawei_push" | "harmony" | "harmonyos" => Some(PushVendor::Hms),
            "xiaomi" | "mi" | "mipush" | "mi_push" => Some(PushVendor::Xiaomi),
            "oppo" | "heytap" | "heytap_push" => Some(PushVendor::Oppo),
            "vivo" | "vivo_push" => Some(PushVendor::Vivo),
            "honor" | "honor_push" => Some(PushVendor::Honor),
            "lenovo" | "lenovo_push" => Some(PushVendor::Lenovo),
            "zte" | "zte_push" => Some(PushVendor::Zte),
            "meizu" | "meizu_push" => Some(PushVendor::Meizu),
            _ => None,
        }
    }
}

/// 推送 Payload
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushPayload {
    pub r#type: String, // "new_message"
    pub conversation_id: u64,
    /// 会话类型（客户端语义：1=单聊，2=群聊）。通知点击回流要用它选对会话页。
    pub channel_type: i32,
    /// 收件人当前的未读总数，用于 iOS 角标。0 = 未知/无未读，此时不下发 badge。
    pub unread_total: i64,
    /// 消息类型（protocol 的 `ContentMessageType` 字符串形式）。
    /// 非文本消息据此渲染 `[图片]` 这类占位符，而不是把原始 content 放进通知。
    pub message_type: String,
    /// 收件人是否允许在通知里显示消息内容。false = 只显示通用文案。
    pub show_preview: bool,
    pub message_id: u64,
    pub sender_id: u64,
    pub content_preview: String,
}

/// Intent 状态（Phase 3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentStatus {
    Pending,    // 待处理
    Processing, // 处理中
    Sent,       // 已发送
    Cancelled,  // 已取消（设备上线）
    Revoked,    // 已撤销（消息撤销）
}

/// PushIntent（设备级，Phase 3.5）
#[derive(Debug, Clone)]
pub struct PushIntent {
    pub intent_id: String,
    pub message_id: u64,
    pub conversation_id: u64,
    pub user_id: u64,
    pub device_id: String, // ✨ Phase 3.5: 设备级 Intent
    pub sender_id: u64,
    pub payload: PushPayload,
    pub created_at: i64,
    pub status: IntentStatus,
}

impl PushIntent {
    pub fn new(
        intent_id: String,
        message_id: u64,
        conversation_id: u64,
        user_id: u64,
        device_id: String, // ✨ Phase 3.5: 新增参数
        sender_id: u64,
        payload: PushPayload,
        created_at: i64,
    ) -> Self {
        Self {
            intent_id,
            message_id,
            conversation_id,
            user_id,
            device_id, // ✨ Phase 3.5: 新增字段
            sender_id,
            payload,
            created_at,
            status: IntentStatus::Pending,
        }
    }
}

/// PushTask（设备级）
#[derive(Debug, Clone)]
pub struct PushTask {
    pub task_id: String,
    pub intent_id: String,
    pub user_id: u64,
    pub device_id: String,
    pub vendor: PushVendor,
    pub push_token: String,
    /// 设备语言（BCP-47）。None = 老客户端没上报，provider 按简体中文兜底。
    pub locale: Option<String>,
    pub payload: PushPayload,
}

/// 推送文案的服务端本地化。
///
/// iOS 的 APNs alert 由系统直接展示，App 完全不参与，所以"用哪种语言"只能在
/// 服务端决定——客户端上报 locale（`privchat_user_devices.locale`），这里按它选词。
///
/// 只有兜底文案需要翻译：消息正文（content_preview）是用户自己发的原文，
/// 不做任何处理。
pub mod locale {
    /// 支持的语言。与客户端 i18n 的四个语言包一一对应。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum PushLocale {
        ZhHans,
        ZhHant,
        English,
        Vietnamese,
    }

    impl PushLocale {
        /// 解析 BCP-47 标签。未知/缺失一律回落简体中文——那是当前的主要用户群，
        /// 也是老客户端（根本不上报 locale）的实际语言。
        ///
        /// 繁体的判定看 script 或地区子标签：`zh-Hant`、`zh-TW`、`zh-HK`、`zh-MO`。
        /// 只看前两位的话，香港用户会拿到简体文案。
        pub fn parse(tag: Option<&str>) -> Self {
            let tag = match tag.map(str::trim).filter(|it| !it.is_empty()) {
                Some(t) => t.to_ascii_lowercase(),
                None => return Self::ZhHans,
            };
            if tag.starts_with("vi") {
                return Self::Vietnamese;
            }
            if tag.starts_with("en") {
                return Self::English;
            }
            if tag.starts_with("zh") {
                let hant = tag.contains("hant")
                    || tag.contains("-tw")
                    || tag.contains("-hk")
                    || tag.contains("-mo");
                return if hant { Self::ZhHant } else { Self::ZhHans };
            }
            Self::ZhHans
        }

        /// 通知标题（没有会话名时的兜底，与客户端 `pushDefaultTitle` 保持一致）。
        pub fn default_title(self) -> &'static str {
            match self {
                Self::ZhHans => "新消息",
                Self::ZhHant => "新訊息",
                Self::English => "New message",
                Self::Vietnamese => "Tin nhắn mới",
            }
        }

        /// 通知正文兜底（content_preview 为空时用，与 `pushDefaultBody` 一致）。
        ///
        /// 也是隐私模式（用户关掉"显示消息预览"）下的唯一正文。
        pub fn default_body(self) -> &'static str {
            match self {
                Self::ZhHans => "你收到一条新消息",
                Self::ZhHant => "你收到一則新訊息",
                Self::English => "You have a new message",
                Self::Vietnamese => "Bạn có một tin nhắn mới",
            }
        }

        /// 非文本消息的类型占位符，如 `[图片]` / `[Photo]`。
        ///
        /// 文本以外的消息**不能**把原始 content 放进通知：那可能是一段 caption、
        /// 一个 URL，也可能是结构化 JSON（系统消息、名片、红包），直接显示要么无意义
        /// 要么泄露内部结构。
        ///
        /// 与客户端 `messagePreviewText` 的分类一一对应；未知类型统一归到"[消息]"，
        /// 不做猜测。
        pub fn preview_for_type(self, message_type: &str) -> &'static str {
            use PushLocale::*;
            match message_type {
                "voice" => match self {
                    ZhHans => "[语音]",
                    ZhHant => "[語音]",
                    English => "[Voice]",
                    Vietnamese => "[Tin nhắn thoại]",
                },
                "image" => match self {
                    ZhHans => "[图片]",
                    ZhHant => "[圖片]",
                    English => "[Photo]",
                    Vietnamese => "[Ảnh]",
                },
                "video" => match self {
                    ZhHans => "[视频]",
                    ZhHant => "[影片]",
                    English => "[Video]",
                    Vietnamese => "[Video]",
                },
                "file" => match self {
                    ZhHans => "[文件]",
                    ZhHant => "[檔案]",
                    English => "[File]",
                    Vietnamese => "[Tệp]",
                },
                "sticker" => match self {
                    ZhHans => "[表情]",
                    ZhHant => "[貼圖]",
                    English => "[Sticker]",
                    Vietnamese => "[Nhãn dán]",
                },
                "contact_card" | "contact" => match self {
                    ZhHans => "[名片]",
                    ZhHant => "[名片]",
                    English => "[Contact]",
                    Vietnamese => "[Danh thiếp]",
                },
                "location" => match self {
                    ZhHans => "[位置]",
                    ZhHant => "[位置]",
                    English => "[Location]",
                    Vietnamese => "[Vị trí]",
                },
                "link" => match self {
                    ZhHans => "[链接]",
                    ZhHant => "[連結]",
                    English => "[Link]",
                    Vietnamese => "[Liên kết]",
                },
                "forward" => match self {
                    ZhHans => "[转发消息]",
                    ZhHant => "[轉發訊息]",
                    English => "[Forwarded]",
                    Vietnamese => "[Tin nhắn chuyển tiếp]",
                },
                "red_packet" => match self {
                    ZhHans => "[红包]",
                    ZhHant => "[紅包]",
                    English => "[Red packet]",
                    Vietnamese => "[Lì xì]",
                },
                "money_transfer" => match self {
                    ZhHans => "[转账]",
                    ZhHant => "[轉帳]",
                    English => "[Transfer]",
                    Vietnamese => "[Chuyển tiền]",
                },
                "system" => match self {
                    ZhHans => "[系统消息]",
                    ZhHant => "[系統訊息]",
                    English => "[System message]",
                    Vietnamese => "[Tin nhắn hệ thống]",
                },
                // text 在调用处就用原文了，不会走到这里；剩下的一律"[消息]"。
                _ => match self {
                    ZhHans => "[消息]",
                    ZhHant => "[訊息]",
                    English => "[Message]",
                    Vietnamese => "[Tin nhắn]",
                },
            }
        }

        /// 按消息类型与隐私设置渲染最终通知正文。
        ///
        /// `show_preview = false`（用户在设置里关掉了消息预览）时**只**返回通用文案。
        /// 这个裁剪必须发生在服务端：APNs 的 alert 由系统直接展示，内容一旦发出去
        /// 就已经在设备上了，客户端没有"收到之后再隐藏"的机会。
        pub fn render_body(self, message_type: &str, content_preview: &str, show_preview: bool) -> String {
            if !show_preview {
                return self.default_body().to_string();
            }
            if message_type == "text" {
                let trimmed = content_preview.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
                return self.default_body().to_string();
            }
            self.preview_for_type(message_type).to_string()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::PushLocale;

        #[test]
        fn parses_language_tags() {
            assert_eq!(PushLocale::parse(Some("en-US")), PushLocale::English);
            assert_eq!(PushLocale::parse(Some("vi")), PushLocale::Vietnamese);
            assert_eq!(PushLocale::parse(Some("zh-Hans-CN")), PushLocale::ZhHans);
        }

        /// 只看前两位的话香港/台湾用户会拿到简体文案。
        #[test]
        fn traditional_chinese_is_detected_by_script_and_region() {
            for tag in ["zh-Hant", "zh-TW", "zh-HK", "zh-MO", "zh-hant-tw"] {
                assert_eq!(PushLocale::parse(Some(tag)), PushLocale::ZhHant, "{}", tag);
            }
        }

        /// 老客户端不上报 locale，不能因此就没有文案。
        #[test]
        fn unknown_and_missing_fall_back_to_simplified_chinese() {
            assert_eq!(PushLocale::parse(None), PushLocale::ZhHans);
            assert_eq!(PushLocale::parse(Some("")), PushLocale::ZhHans);
            assert_eq!(PushLocale::parse(Some("ko-KR")), PushLocale::ZhHans);
        }
    }
}
