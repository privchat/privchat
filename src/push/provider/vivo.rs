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

/// Vivo Push Provider
pub struct VivoProvider {
    client: Client,
    app_id: String,
    access_token: String,
    endpoint: String,
}

impl VivoProvider {
    pub fn new(app_id: String, access_token: String, endpoint: Option<String>) -> Self {
        let endpoint = endpoint
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("https://api-push.vivo.com.cn")
            .trim_end_matches('/')
            .to_string();
        Self {
            client: Client::new(),
            app_id,
            access_token,
            endpoint,
        }
    }

    fn build_payload(&self, task: &PushTask) -> serde_json::Value {
        json!({
            "regIds": [task.push_token],
            "notification": {
                "title": "新消息",
                "content": task.payload.content_preview
            },
            "params": {
                "type": task.payload.r#type,
                "conversation_id": task.payload.conversation_id,
                "message_id": task.payload.message_id,
                "sender_id": task.payload.sender_id,
                "app_id": self.app_id
            }
        })
    }
}

#[async_trait]
impl PushProvider for VivoProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        let url = format!("{}/message/send", self.endpoint);
        let payload = self.build_payload(task);

        info!(
            "[VIVO] Sending push: task_id={}, user_id={}, device_id={}",
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
            .map_err(|e| ServerError::Internal(format!("Vivo request failed: {}", e)))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            info!("[VIVO] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            error!(
                "[VIVO] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, body
            );
            Err(ServerError::Internal(format!(
                "Vivo push failed: status={}, error={}",
                status, body
            )))
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Vivo
    }
}
