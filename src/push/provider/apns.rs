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
use serde_json::json;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info};

/// APNs (Apple Push Notification service) Provider
///
/// 使用 APNs HTTP/2 API
/// 推送的存活时间：一天。超过这个时间还没送达的消息，补投的价值已经低于打扰。
const PUSH_TTL_SECS: u64 = 24 * 60 * 60;

pub struct ApnsProvider {
    client: Client,
    bundle_id: String,
    team_id: String,
    key_id: String,
    private_key: EncodingKey,
    use_sandbox: bool,
    /// 缓存的 provider token 与它的签发时刻（epoch 秒）。
    ///
    /// Apple 要求同一个 provider token 至少复用 20 分钟、最多 60 分钟：每条推送都重签
    /// 会撞上 429 `TooManyProviderTokenUpdates`，那时**所有**推送一起失败。
    cached_token: Mutex<Option<(String, u64)>>,
}

impl ApnsProvider {
    /// 创建新的 APNs Provider
    ///
    /// # 参数
    /// - bundle_id: App Bundle ID
    /// - team_id: Apple Developer Team ID
    /// - key_id: APNs Key ID
    /// - private_key_path: 私钥文件路径（.p8 文件）
    /// - use_sandbox: 是否使用 APNs Sandbox（开发环境）
    pub fn new(
        bundle_id: String,
        team_id: String,
        key_id: String,
        private_key_path: &str,
        use_sandbox: bool,
    ) -> Result<Self> {
        // 读取私钥文件
        let private_key_content = std::fs::read_to_string(private_key_path).map_err(|e| {
            ServerError::Internal(format!("Failed to read APNs private key: {}", e))
        })?;

        let private_key =
            EncodingKey::from_ec_pem(private_key_content.as_bytes()).map_err(|e| {
                ServerError::Internal(format!("Failed to parse APNs private key: {}", e))
            })?;

        Ok(Self {
            client: Client::new(),
            bundle_id,
            team_id,
            key_id,
            private_key,
            use_sandbox,
            cached_token: Mutex::new(None),
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

        // 复用窗口取 50 分钟：低于 Apple 的 60 分钟上限，高于 20 分钟下限。
        const REUSE_WINDOW_SECS: u64 = 50 * 60;
        if let Ok(guard) = self.cached_token.lock() {
            if let Some((token, issued_at)) = guard.as_ref() {
                if now.saturating_sub(*issued_at) < REUSE_WINDOW_SECS {
                    return Ok(token.clone());
                }
            }
        }

        // APNs JWT Claims
        let claims = json!({
            "iss": self.team_id,
            "iat": now
        });

        let mut header = Header::new(Algorithm::ES256);
        // 🔴 kid 不是可选的：header 里没有它，Apple 一律回 403 InvalidProviderToken，
        // 且错误里不会告诉你少的是哪一项。
        header.kid = Some(self.key_id.clone());

        let token = encode(&header, &claims, &self.private_key)
            .map_err(|e| ServerError::Internal(format!("Failed to generate APNs JWT: {}", e)))?;
        if let Ok(mut guard) = self.cached_token.lock() {
            *guard = Some((token.clone(), now));
        }
        Ok(token)
    }

    /// 构建 APNs 消息 payload
    fn build_apns_payload(task: &PushTask) -> serde_json::Value {
        // 语言由设备上报（privchat_user_devices.locale）。iOS 的 alert 是系统直接
        // 展示的，App 没有机会本地化，所以只能在这里定。
        let locale = crate::push::types::locale::PushLocale::parse(task.locale.as_deref());
        // 非文本消息渲染成 `[图片]` 这类占位符；用户关掉预览时只给通用文案。
        // 裁剪必须在这里做完——alert 一旦发出去就已经在设备上了。
        let body = locale.render_body(
            &task.payload.message_type,
            &task.payload.content_preview,
            task.payload.show_preview,
        );
        let mut aps = json!({
            "alert": {
                "title": locale.default_title(),
                "body": body
            },

            // 同一个会话的多条推送在锁屏上折叠成一条，而不是堆成一列。
            "thread-id": task.payload.conversation_id.to_string(),
        });
        // 声音开关必须在这里生效：通知由 iOS 展示，App 拦不住自己的远程通知。
        // 不带 sound 字段 = 静默通知（仍然显示横幅，只是不响）。
        if task.push_sound {
            aps["sound"] = json!("default");
        }
        // badge 以前写死 1：手机上有 20 条未读，角标也只显示 1。
        // 未知（0）时干脆不带这个字段——带 0 会把角标清掉，比不准更糟。
        if task.payload.unread_total > 0 {
            aps["badge"] = json!(task.payload.unread_total);
        }
        json!({
            "aps": aps,
            "data": {
                "type": task.payload.r#type,
                "conversation_id": task.payload.conversation_id.to_string(),
                "channel_type": task.payload.channel_type.to_string(),
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
        let endpoint = if self.use_sandbox {
            "https://api.sandbox.push.apple.com"
        } else {
            "https://api.push.apple.com"
        };
        let url = format!("{}/3/device/{}", endpoint, task.push_token);

        // 3. 构建 payload
        let payload = Self::build_apns_payload(task);
        let expiration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            + PUSH_TTL_SECS;
        // collapse-id 上限 64 字节，会话 id 远在其内。
        let collapse_id = format!("conv-{}", task.payload.conversation_id);

        info!(
            "[APNs] Sending push: task_id={}, user_id={}, device_id={}",
            task.task_id, task.user_id, task.device_id
        );

        // 4. 发送 HTTP/2 请求
        let response = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {}", jwt_token))
            .header("apns-topic", &self.bundle_id)
            .header("apns-priority", "10")
            .header("apns-push-type", "alert")
            // 一条消息只有一次机会：设备离线超过一天再上线时，补投一堆隔夜通知
            // 只会淹掉当下真正要看的东西。到点 Apple 自己丢弃。
            .header("apns-expiration", expiration.to_string())
            // 同会话去重：Apple 只保留同一个 collapse-id 的最后一条。锁屏上因此
            // 是"这个会话有新消息"，而不是同一个人刷屏刷出二十条通知。
            .header("apns-collapse-id", collapse_id)
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
            let reason = serde_json::from_str::<serde_json::Value>(&error_text)
                .ok()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_string));

            // 这几种是「这个 token 永远不会再成功」：设备卸载了、token 属于别的
            // 环境/bundle、或者压根不是个合法 token。继续留着它，只会每来一条消息
            // 就再撞一次 Apple 的限流。
            //
            // 410 Unregistered 是最常见的一种：用户卸载 App 之后 Apple 就这么回。
            let token_dead = matches!(
                reason.as_deref(),
                Some("BadDeviceToken")
                    | Some("Unregistered")
                    | Some("DeviceTokenNotForTopic")
                    | Some("ExpiredToken")
            ) || status.as_u16() == 410;

            let error_msg = match &reason {
                Some(reason) => format!("APNs error: {} ({})", reason, status),
                None => format!("APNs push failed: status={}, error={}", status, error_text),
            };

            if token_dead {
                Err(ServerError::PushTokenInvalid(error_msg))
            } else {
                Err(ServerError::Internal(error_msg))
            }
        }
    }

    fn vendor(&self) -> PushVendor {
        PushVendor::Apns
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
            vendor: PushVendor::Apns,
            push_token: "tok".into(),
            locale: Some("zh-Hans".into()),
            push_sound: true,
            payload: crate::push::types::PushPayload {
                r#type: "new_message".into(),
                conversation_id: 1234,
                channel_type,
                message_id: 99,
                sender_id: 5,
                content_preview: "hi".into(),
                unread_total: 7,
                message_type: "text".into(),
                show_preview: true,
            },
        }
    }

    /// 点击回流靠 `data.conversation_id` + `data.channel_type`；少一个就跳不到会话。
    #[test]
    fn apns_payload_carries_navigation_fields() {
        let payload = ApnsProvider::build_apns_payload(&task(2));
        assert_eq!(payload["data"]["conversation_id"], "1234");
        assert_eq!(payload["data"]["channel_type"], "2");
        assert_eq!(payload["aps"]["alert"]["body"], "hi");
        assert_eq!(payload["aps"]["thread-id"], "1234");
    }

    /// iOS 的 alert 由系统展示，App 没机会本地化：语言必须在服务端定。
    #[test]
    fn apns_title_follows_device_locale() {
        let mut t = task(1);
        t.locale = Some("vi".into());
        let payload = ApnsProvider::build_apns_payload(&t);
        assert_eq!(payload["aps"]["alert"]["title"], "Tin nhắn mới");

        t.locale = Some("zh-TW".into());
        let payload = ApnsProvider::build_apns_payload(&t);
        assert_eq!(payload["aps"]["alert"]["title"], "新訊息");
    }

    /// 空正文（比如纯附件消息）不能推出一条空白通知。
    #[test]
    fn apns_body_falls_back_when_preview_empty() {
        let mut t = task(1);
        t.locale = Some("en".into());
        t.payload.content_preview = "   ".into();
        let payload = ApnsProvider::build_apns_payload(&t);
        assert_eq!(payload["aps"]["alert"]["body"], "You have a new message");
    }

    /// 声音开关在 payload 里：通知由 iOS 展示，App 拦不住自己的远程通知，
    /// 所以"关掉声音"只能靠服务端不下发 sound 字段。
    #[test]
    fn apns_omits_sound_when_device_muted_it() {
        let mut t = task(1);
        assert_eq!(ApnsProvider::build_apns_payload(&t)["aps"]["sound"], "default");

        t.push_sound = false;
        let payload = ApnsProvider::build_apns_payload(&t);
        assert!(
            payload["aps"].get("sound").is_none(),
            "设备关掉了提示音，payload 里仍带 sound: {}",
            payload
        );
        // 静音不等于不显示：横幅还是要有的。
        assert_eq!(payload["aps"]["alert"]["body"], "hi");
    }

    /// 非文本消息不能把原始 content 塞进通知：那可能是 caption、URL 或结构化 JSON。
    #[test]
    fn apns_renders_type_placeholder_for_non_text() {
        let mut t = task(1);
        t.payload.message_type = "file".into();
        t.payload.content_preview = "s3://bucket/secret-contract.pdf".into();

        t.locale = Some("zh-Hans".into());
        assert_eq!(
            ApnsProvider::build_apns_payload(&t)["aps"]["alert"]["body"],
            "[文件]"
        );
        t.locale = Some("en".into());
        assert_eq!(
            ApnsProvider::build_apns_payload(&t)["aps"]["alert"]["body"],
            "[File]"
        );
        t.locale = Some("vi".into());
        assert_eq!(
            ApnsProvider::build_apns_payload(&t)["aps"]["alert"]["body"],
            "[Tệp]"
        );
    }

    /// 关掉"显示消息预览"之后，正文里不能留下任何原文痕迹——APNs 的 alert 由系统
    /// 展示，发出去就已经在设备上了，客户端没有事后隐藏的机会。
    #[test]
    fn apns_hides_content_when_preview_disabled() {
        let mut t = task(1);
        t.payload.show_preview = false;
        t.payload.content_preview = "明天上午十点开会".into();
        let payload = ApnsProvider::build_apns_payload(&t);
        assert_eq!(payload["aps"]["alert"]["body"], "你收到一条新消息");

        // 整个 payload 里都不该出现原文。
        let serialized = payload.to_string();
        assert!(
            !serialized.contains("明天上午十点开会"),
            "隐私模式下 payload 仍然带了原文: {}",
            serialized
        );
    }

    /// badge 写死 1 时，手机上二十条未读也只显示 1。
    #[test]
    fn apns_badge_uses_real_unread_total() {
        let payload = ApnsProvider::build_apns_payload(&task(1));
        assert_eq!(payload["aps"]["badge"], 7);
    }

    /// 未知未读数（0）时**不能**下发 badge：带 0 会把角标清掉，比不准更糟。
    #[test]
    fn apns_omits_badge_when_unread_unknown() {
        let mut t = task(1);
        t.payload.unread_total = 0;
        let payload = ApnsProvider::build_apns_payload(&t);
        assert!(payload["aps"].get("badge").is_none());
    }
}
