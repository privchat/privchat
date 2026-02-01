use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tracing::{info, error};
use jsonwebtoken::{encode, Header, EncodingKey, Algorithm};
use std::time::{SystemTime, UNIX_EPOCH};
use crate::push::provider::provider_trait::PushProvider;
use crate::push::types::{PushTask, PushVendor};
use crate::error::{Result, ServerError};

/// APNs (Apple Push Notification service) Provider
/// 
/// 使用 APNs HTTP/2 API
pub struct ApnsProvider {
    client: Client,
    bundle_id: String,
    team_id: String,
    key_id: String,
    private_key: EncodingKey,
}

impl ApnsProvider {
    /// 创建新的 APNs Provider
    /// 
    /// # 参数
    /// - bundle_id: App Bundle ID
    /// - team_id: Apple Developer Team ID
    /// - key_id: APNs Key ID
    /// - private_key_path: 私钥文件路径（.p8 文件）
    pub fn new(
        bundle_id: String,
        team_id: String,
        key_id: String,
        private_key_path: &str,
    ) -> Result<Self> {
        // 读取私钥文件
        let private_key_content = std::fs::read_to_string(private_key_path)
            .map_err(|e| ServerError::Internal(format!("Failed to read APNs private key: {}", e)))?;
        
        let private_key = EncodingKey::from_ec_pem(private_key_content.as_bytes())
            .map_err(|e| ServerError::Internal(format!("Failed to parse APNs private key: {}", e)))?;
        
        Ok(Self {
            client: Client::new(),
            bundle_id,
            team_id,
            key_id,
            private_key,
        })
    }
    
    /// 生成 APNs JWT Token
    /// 
    /// APNs 使用 JWT Token 进行认证，Token 有效期为 1 小时
    fn generate_jwt_token(&self) -> Result<String> {
        use serde_json::json;
        
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        
        // APNs JWT Claims
        let claims = json!({
            "iss": self.team_id,
            "iat": now
        });
        
        let header = Header::new(Algorithm::ES256);
        
        encode(&header, &claims, &self.private_key)
            .map_err(|e| ServerError::Internal(format!("Failed to generate APNs JWT: {}", e)))
    }
    
    /// 构建 APNs 消息 payload
    fn build_apns_payload(&self, task: &PushTask) -> serde_json::Value {
        json!({
            "aps": {
                "alert": {
                    "title": "新消息",
                    "body": task.payload.content_preview
                },
                "badge": 1,
                "sound": "default"
            },
            "data": {
                "type": task.payload.r#type,
                "conversation_id": task.payload.conversation_id.to_string(),
                "message_id": task.payload.message_id.to_string(),
                "sender_id": task.payload.sender_id.to_string(),
            }
        })
    }
}

#[async_trait]
impl PushProvider for ApnsProvider {
    async fn send(&self, task: &PushTask) -> Result<()> {
        // 1. 生成 JWT Token
        let jwt_token = self.generate_jwt_token()?;
        
        // 2. 构建 APNs URL
        // 生产环境: https://api.push.apple.com
        // 开发环境: https://api.sandbox.push.apple.com
        let url = format!(
            "https://api.push.apple.com/3/device/{}",
            task.push_token
        );
        
        // 3. 构建 payload
        let payload = self.build_apns_payload(task);
        
        info!(
            "[APNs] Sending push: task_id={}, user_id={}, device_id={}",
            task.task_id, task.user_id, task.device_id
        );
        
        // 4. 发送 HTTP/2 请求
        let response = self.client
            .post(&url)
            .header("authorization", format!("bearer {}", jwt_token))
            .header("apns-topic", &self.bundle_id)
            .header("apns-priority", "10")
            .header("apns-push-type", "alert")
            .json(&payload)
            .send()
            .await
            .map_err(|e| ServerError::Internal(format!("APNs request failed: {}", e)))?;
        
        let status = response.status();
        if status.is_success() {
            info!("[APNs] Push sent successfully: task_id={}", task.task_id);
            Ok(())
        } else {
            let error_text = response.text().await.unwrap_or_default();
            error!(
                "[APNs] Push failed: task_id={}, status={}, error={}",
                task.task_id, status, error_text
            );
            
            // 解析 APNs 错误码
            let error_msg = if let Ok(error_json) = serde_json::from_str::<serde_json::Value>(&error_text) {
                if let Some(reason) = error_json.get("reason").and_then(|r| r.as_str()) {
                    format!("APNs error: {} ({})", reason, status)
                } else {
                    format!("APNs push failed: status={}", status)
                }
            } else {
                format!("APNs push failed: status={}, error={}", status, error_text)
            };
            
            Err(ServerError::Internal(error_msg))
        }
    }
    
    fn vendor(&self) -> PushVendor {
        PushVendor::Apns
    }
}
