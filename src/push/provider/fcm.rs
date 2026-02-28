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

use crate::error::{Result, ServerError};
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tracing::{error, info};

/// FCM (Firebase Cloud Messaging) Provider
///
/// 使用 FCM HTTP v1 API
pub struct FcmProvider {
    client: Client,
    project_id: String,
    access_token: String, // OAuth 2.0 access token
}

impl FcmProvider {
    /// 创建新的 FCM Provider
    ///
    /// # 参数
    /// - project_id: Firebase 项目 ID
    /// - access_token: OAuth 2.0 access token（从 service account 获取）
    pub fn new(project_id: String, access_token: String) -> Self {
        Self {
            client: Client::new(),
            project_id,
            access_token,
        }
    }

    /// 构建 FCM 消息 payload
    fn build_fcm_payload(&self, task: &PushTask) -> serde_json::Value {
        json!({
            "message": {
                "token": task.push_token,
                "notification": {
                    "title": "新消息",
                    "body": task.payload.content_preview
                },
                "data": {
                    "type": task.payload.r#type,
                    "conversation_id": task.payload.conversation_id.to_string(),
                    "message_id": task.payload.message_id.to_string(),
                    "sender_id": task.payload.sender_id.to_string(),
                },
                "android": {
                    "priority": "high"
                },
                "apns": {
                    "headers": {
                        "apns-priority": "10"
                    }
                }
            }
        })
    }
}

#[async_trait]
impl PushProvider for FcmProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project_id
        );

        let payload = self.build_fcm_payload(task);

        info!(
            "[FCM] Sending push: task_id={}, user_id={}, device_id={}",
            task.task_id, task.user_id, task.device_id
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ServerError::Internal(format!("FCM request failed: {}", e)))?;

        let status = response.status();
        if status.is_success() {
            info!("[FCM] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            let error_text = response.text().await.unwrap_or_default();
            error!(
                "[FCM] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, error_text
            );
            Err(ServerError::Internal(format!(
                "FCM push failed: status={}, error={}",
                status, error_text
            )))
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Fcm
    }
}
