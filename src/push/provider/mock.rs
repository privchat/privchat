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

use crate::error::Result;
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use async_trait::async_trait;
use tracing::info;

/// Mock Provider（用于测试和 MVP 阶段）
///
/// 不调用真实 API，只打印日志
pub struct MockProvider;

#[async_trait]
impl PushProvider for MockProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        info!(
            "[MOCK PUSH] Sending push: intent_id={}, task_id={}, user_id={}, device_id={}, vendor={:?}",
            task.intent_id, task.task_id, task.user_id, task.device_id, task.vendor
        );
        info!(
            "[MOCK PUSH] Payload: type={}, conversation_id={}, message_id={}, sender_id={}, preview={}",
            task.payload.r#type, 
            task.payload.conversation_id, 
            task.payload.message_id, 
            task.payload.sender_id,
            task.payload.content_preview
        );
        Ok(())
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Apns // Mock 返回 Apns，实际不调用
    }
}
