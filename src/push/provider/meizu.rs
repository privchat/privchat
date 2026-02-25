use crate::error::{Result, ServerError};
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tracing::{error, info};

/// Meizu Push Provider
pub struct MeizuProvider {
    client: Client,
    app_id: String,
    access_token: String,
    endpoint: String,
}

impl MeizuProvider {
    pub fn new(app_id: String, access_token: String, endpoint: Option<String>) -> Self {
        let endpoint = endpoint
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("https://server-api-push.meizu.com")
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
            "push_ids": [task.push_token],
            "title": "新消息",
            "content": task.payload.content_preview,
            "payload": {
                "type": task.payload.r#type,
                "conversation_id": task.payload.conversation_id,
                "message_id": task.payload.message_id,
                "sender_id": task.payload.sender_id,
            }
        })
    }
}

#[async_trait]
impl PushProvider for MeizuProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        let url = format!("{}/garcia/api/server/push/varnished/pushByPushId", self.endpoint);
        let payload = self.build_payload(task);

        info!(
            "[MEIZU] Sending push: task_id={}, user_id={}, device_id={}",
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
            .map_err(|e| ServerError::Internal(format!("Meizu request failed: {}", e)))?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            info!("[MEIZU] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            error!(
                "[MEIZU] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, body
            );
            Err(ServerError::Internal(format!(
                "Meizu push failed: status={}, error={}",
                status, body
            )))
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Meizu
    }
}
