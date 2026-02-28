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

/// HMS (Huawei Mobile Services) Push Provider
///
/// 使用 Huawei Push Kit HTTP API。
pub struct HmsProvider {
    client: Client,
    app_id: String,
    access_token: String,
    endpoint: String,
}

impl HmsProvider {
    /// 创建 HMS Provider
    ///
    /// # 参数
    /// - app_id: 华为应用 App ID
    /// - access_token: OAuth 2.0 Access Token
    /// - endpoint: Push API 域名，默认 `https://push-api.cloud.huawei.com`
    pub fn new(app_id: String, access_token: String, endpoint: Option<String>) -> Self {
        let endpoint = endpoint
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("https://push-api.cloud.huawei.com")
            .trim_end_matches('/')
            .to_string();
        Self {
            client: Client::new(),
            app_id,
            access_token,
            endpoint,
        }
    }

    fn build_hms_payload(&self, task: &PushTask) -> serde_json::Value {
        let data = json!({
            "type": task.payload.r#type,
            "conversation_id": task.payload.conversation_id,
            "message_id": task.payload.message_id,
            "sender_id": task.payload.sender_id,
        });

        json!({
            "validate_only": false,
            "message": {
                "token": [task.push_token],
                "android": {
                    "notification": {
                        "title": "新消息",
                        "body": task.payload.content_preview
                    },
                    "data": data.to_string()
                }
            }
        })
    }
}

#[async_trait]
impl PushProvider for HmsProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        let url = format!("{}/v1/{}/messages:send", self.endpoint, self.app_id);
        let payload = self.build_hms_payload(task);

        info!(
            "[HMS] Sending push: task_id={}, user_id={}, device_id={}",
            task.task_id, task.user_id, task.device_id
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.access_token))
            .header("Content-Type", "application/json; charset=UTF-8")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ServerError::Internal(format!("HMS request failed: {}", e)))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            info!("[HMS] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            error!(
                "[HMS] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, body
            );
            Err(ServerError::Internal(format!(
                "HMS push failed: status={}, error={}",
                status, body
            )))
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Hms
    }
}
