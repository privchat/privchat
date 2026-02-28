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

/// Domain Events（领域事件）
///
/// Phase 3: 添加撤销和取消推送事件
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DomainEvent {
    /// 消息已提交（已落库）
    MessageCommitted {
        message_id: u64,
        conversation_id: u64,
        sender_id: u64,
        recipient_id: u64,       // 接收者 ID
        content_preview: String, // 内容预览（用于推送显示）
        message_type: String,
        timestamp: i64,
        device_id: Option<String>, // ✨ Phase 3.5: 可选的设备ID（设备级 Intent）
    },

    /// 消息已撤销（Phase 3）
    MessageRevoked {
        message_id: u64,
        conversation_id: u64,
        revoker_id: u64, // 撤销者 ID
        timestamp: i64,
    },

    /// 用户上线（Phase 3）
    UserOnline {
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },

    /// 消息已通过长连接成功送达（Phase 3.5）
    MessageDelivered {
        message_id: u64,
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },

    /// 设备上线（Phase 3.5，替代 UserOnline 用于设备级取消）
    DeviceOnline {
        user_id: u64,
        device_id: String,
        timestamp: i64,
    },
}
