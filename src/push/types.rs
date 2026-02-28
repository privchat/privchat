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
            "hms" | "huawei" | "huawei_push" | "harmony" | "harmonyos" => {
                Some(PushVendor::Hms)
            }
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
    pub payload: PushPayload,
}
