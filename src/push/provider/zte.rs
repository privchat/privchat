use crate::error::{Result, ServerError};
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tracing::{error, info};

/// ZTE Push Provider
pub struct ZteProvider {
    client: Client,
    app_id: String,
    access_token: String,
    endpoint: String,
}

impl ZteProvider {
    pub fn new(app_id: String, access_token: String, endpoint: Option<String>) -> Self {
        let endpoint = endpoint
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("https://push-api.ztedevice.com")
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
            "app_id": self.app_id,
            "token": task.push_token,
            "title": "新消息",
            "body": task.payload.content_preview,
            "data": {
                "type": task.payload.r#type,
                "conversation_id": task.payload.conversation_id,
                "message_id": task.payload.message_id,
                "sender_id": task.payload.sender_id,
            }
        })
    }
}

#[async_trait]
impl PushProvider for ZteProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        let url = format!("{}/v1/{}/messages:send", self.endpoint, self.app_id);
        let payload = self.build_payload(task);

        info!(
            "[ZTE] Sending push: task_id={}, user_id={}, device_id={}",
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
            .map_err(|e| ServerError::Internal(format!("ZTE request failed: {}", e)))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            info!("[ZTE] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            error!(
                "[ZTE] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, body
            );
            Err(ServerError::Internal(format!(
                "ZTE push failed: status={}, error={}",
                status, body
            )))
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Zte
    }
}
