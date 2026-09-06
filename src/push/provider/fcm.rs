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
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

/// FCM 服务账号（Firebase 控制台 → 项目设置 → 服务账号 → 生成新的私钥）。
///
/// 只取签名换 token 用得到的四个字段；文件里其余字段忽略。
#[derive(Debug, Clone, Deserialize)]
struct ServiceAccount {
    project_id: String,
    client_email: String,
    private_key: String,
    #[serde(default = "default_token_uri")]
    token_uri: String,
}

fn default_token_uri() -> String {
    "https://oauth2.googleapis.com/token".to_string()
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
}

/// 缓存的 OAuth2 access token。`expires_at` 是绝对 epoch 秒。
#[derive(Debug, Clone)]
struct CachedToken {
    value: String,
    expires_at: u64,
}

/// 凭据来源。
enum Credentials {
    /// 服务账号：自己签 JWT 换 access token，到期自动续。生产唯一正确形态。
    ServiceAccount {
        account: ServiceAccount,
        key: EncodingKey,
        cache: Mutex<Option<CachedToken>>,
    },
    /// 手工粘贴的 access token。**一小时后必然失效且不会自愈**，只用于本地联调，
    /// 启动时会打警告。
    StaticToken(String),
}

pub struct FcmProvider {
    client: Client,
    project_id: String,
    credentials: Arc<Credentials>,
}

impl FcmProvider {
    /// 从服务账号 JSON 文件构造（推荐）。`project_id` 缺省时取文件里的。
    pub fn from_service_account_file(path: &str, project_id_override: Option<String>) -> Result<Self> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            ServerError::Internal(format!("读取 FCM 服务账号文件失败 ({}): {}", path, e))
        })?;
        let account: ServiceAccount = serde_json::from_str(&raw).map_err(|e| {
            ServerError::Internal(format!("解析 FCM 服务账号 JSON 失败 ({}): {}", path, e))
        })?;
        // 服务账号私钥是 PKCS#8 RSA。密钥坏掉要在启动期就炸，不能拖到第一条推送。
        let key = EncodingKey::from_rsa_pem(account.private_key.as_bytes()).map_err(|e| {
            ServerError::Internal(format!("解析 FCM 服务账号私钥失败: {}", e))
        })?;
        let project_id = project_id_override
            .map(|it| it.trim().to_string())
            .filter(|it| !it.is_empty())
            .unwrap_or_else(|| account.project_id.clone());
        Ok(Self {
            client: Client::new(),
            project_id,
            credentials: Arc::new(Credentials::ServiceAccount {
                account,
                key,
                cache: Mutex::new(None),
            }),
        })
    }

    /// 用现成的 access token 构造（仅联调）。
    pub fn new(project_id: String, access_token: String) -> Self {
        warn!(
            "[FCM] 使用静态 access_token：OAuth2 token 有效期只有 1 小时，过期后推送会持续 401 \
             且不会自动恢复。生产请改配 push.fcm.service_account_path"
        );
        Self {
            client: Client::new(),
            project_id,
            credentials: Arc::new(Credentials::StaticToken(access_token)),
        }
    }

    fn now_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// 取当前可用的 access token。服务账号模式下按需换取并缓存，
    /// 过期前 120s 就提前续，避免"刚拿到就过期"的边界。
    async fn access_token(&self) -> Result<String> {
        match &*self.credentials {
            Credentials::StaticToken(token) => Ok(token.clone()),
            Credentials::ServiceAccount { account, key, cache } => {
                let now = Self::now_secs();
                {
                    let guard = cache.lock().await;
                    if let Some(cached) = guard.as_ref() {
                        if cached.expires_at > now + 120 {
                            return Ok(cached.value.clone());
                        }
                    }
                }
                // 锁外发网络请求会让并发推送各换一次 token；这里持锁换取，
                // 后到的协程醒来时缓存已经新鲜，直接复用。
                let mut guard = cache.lock().await;
                if let Some(cached) = guard.as_ref() {
                    if cached.expires_at > now + 120 {
                        return Ok(cached.value.clone());
                    }
                }
                let fetched = self.fetch_access_token(account, key).await?;
                let value = fetched.value.clone();
                *guard = Some(fetched);
                Ok(value)
            }
        }
    }

    /// 用服务账号私钥签一个 JWT，向 Google OAuth2 换 access token。
    async fn fetch_access_token(
        &self,
        account: &ServiceAccount,
        key: &EncodingKey,
    ) -> Result<CachedToken> {
        let now = Self::now_secs();
        let claims = json!({
            "iss": account.client_email,
            "scope": "https://www.googleapis.com/auth/firebase.messaging",
            "aud": account.token_uri,
            "iat": now,
            "exp": now + 3600,
        });
        let mut header = Header::new(Algorithm::RS256);
        header.typ = Some("JWT".to_string());
        let assertion = encode(&header, &claims, key)
            .map_err(|e| ServerError::Internal(format!("FCM JWT 签名失败: {}", e)))?;

        let response = self
            .client
            .post(&account.token_uri)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await
            .map_err(|e| ServerError::Internal(format!("FCM token 请求失败: {}", e)))?;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(ServerError::Internal(format!(
                "FCM token 获取失败: status={}, error={}",
                status, text
            )));
        }
        let parsed: TokenResponse = serde_json::from_str(&text)
            .map_err(|e| ServerError::Internal(format!("FCM token 响应解析失败: {}", e)))?;
        info!("[FCM] OAuth2 access token 已刷新，{}s 后过期", parsed.expires_in);
        Ok(CachedToken {
            value: parsed.access_token,
            expires_at: Self::now_secs() + parsed.expires_in,
        })
    }

    /// FCM 只发 **data-only** 消息，不带 `notification` 块。
    ///
    /// 带 notification 的话，App 在后台时由系统直接弹通知、进程完全不参与：点击只能拉起
    /// 启动页，落不到具体会话，也没法跟本地通知合并成同一条。data-only 则始终走
    /// `onMessageReceived`，由客户端复用既有的 NotificationPresenter（channel、会话合并、
    /// 点击回流都是现成的）。代价是 App 被用户强杀后收不到——那属于厂商通道的范畴。
    fn build_fcm_payload(task: &PushTask) -> serde_json::Value {
        json!({
            "message": {
                "token": task.push_token,
                "data": {
                    "type": task.payload.r#type,
                    "conversation_id": task.payload.conversation_id.to_string(),
                    "channel_type": task.payload.channel_type.to_string(),
                    "message_id": task.payload.message_id.to_string(),
                    "sender_id": task.payload.sender_id.to_string(),
                    "content_preview": task.payload.content_preview.clone(),
                },
                "android": {
                    // high 才能在 Doze 下即时唤醒；normal 会被系统攒着批量投递。
                    // data-only 消息也只有 high 优先级才保证唤醒进程。
                    "priority": "high",
                    // 与 APNs 的 apns-expiration 同义：离线一天以上的消息不再补投，
                    // 否则用户开机会被隔夜通知淹没。
                    "ttl": "86400s",
                    // 同会话折叠，FCM 只保留最后一条。设备离线期间同一个会话来了
                    // 十条消息，上线时不该收到十条通知。
                    "collapse_key": format!("conv-{}", task.payload.conversation_id)
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

        let access_token = self.access_token().await?;
        let payload = Self::build_fcm_payload(task);

        info!(
            "[FCM] Sending push: task_id={}, user_id={}, device_id={}",
            task.task_id, task.user_id, task.device_id
        );

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", access_token))
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

            // FCM v1 用 error.status 表达失败原因：
            // - UNREGISTERED：App 被卸载 / token 轮换过，这个 token 已经死了
            // - INVALID_ARGUMENT + 404：token 格式非法或不属于本项目
            // 其余（UNAVAILABLE / INTERNAL / 限流）是临时故障，token 还是好的。
            let fcm_status = serde_json::from_str::<serde_json::Value>(&error_text)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.get("status"))
                        .and_then(|s| s.as_str())
                        .map(str::to_string)
                });
            let token_dead = matches!(fcm_status.as_deref(), Some("UNREGISTERED"))
                || (status.as_u16() == 404)
                || (status.as_u16() == 400
                    && matches!(fcm_status.as_deref(), Some("INVALID_ARGUMENT")));

            let message = format!("FCM push failed: status={}, error={}", status, error_text);
            if token_dead {
                Err(ServerError::PushTokenInvalid(message))
            } else {
                Err(ServerError::Internal(message))
            }
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Fcm
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(channel_type: i32) -> PushTask {
        PushTask {
            task_id: "t1".into(),
            intent_id: "i1".into(),
            user_id: 7,
            device_id: "d1".into(),
            vendor: PushVendor::Fcm,
            push_token: "tok".into(),
            payload: crate::push::types::PushPayload {
                r#type: "new_message".into(),
                conversation_id: 1234,
                channel_type,
                message_id: 99,
                sender_id: 5,
                content_preview: "hi".into(),
                unread_total: 7,
            },
        }
    }

    /// data-only 是刻意的：带 notification 块时后台推送由系统直接展示，
    /// 客户端拿不到 conversation_id，点击只能落在启动页。
    #[test]
    fn fcm_payload_is_data_only() {
        let payload = FcmProvider::build_fcm_payload(&task(2));
        let message = &payload["message"];
        assert!(
            message.get("notification").is_none(),
            "FCM payload 不该带 notification 块，否则点击回流会失效"
        );
        assert_eq!(message["data"]["conversation_id"], "1234");
        assert_eq!(message["data"]["channel_type"], "2");
        assert_eq!(message["data"]["content_preview"], "hi");
        assert_eq!(message["android"]["priority"], "high");
        assert_eq!(message["android"]["collapse_key"], "conv-1234");
        assert_eq!(message["android"]["ttl"], "86400s");
    }

    /// UNREGISTERED = App 已卸载 / token 轮换过，必须清库；
    /// UNAVAILABLE 是 Google 侧临时故障，清掉的话用户会平白丢掉推送能力。
    #[test]
    fn only_unregistered_is_treated_as_dead_token() {
        let dead = r#"{"error":{"status":"UNREGISTERED","message":"..."}}"#;
        let transient = r#"{"error":{"status":"UNAVAILABLE","message":"..."}}"#;
        assert!(fcm_status_of(dead).as_deref() == Some("UNREGISTERED"));
        assert!(fcm_status_of(transient).as_deref() == Some("UNAVAILABLE"));
    }

    fn fcm_status_of(body: &str) -> Option<String> {
        serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("error")
                    .and_then(|e| e.get("status"))
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
            })
    }

    /// FCM 的 data 值必须全是字符串——传数字会被 FCM 直接拒掉整条请求。
    #[test]
    fn fcm_data_values_are_all_strings() {
        let payload = FcmProvider::build_fcm_payload(&task(1));
        for (key, value) in payload["message"]["data"].as_object().unwrap() {
            assert!(value.is_string(), "data.{} 不是字符串: {}", key, value);
        }
    }
}
