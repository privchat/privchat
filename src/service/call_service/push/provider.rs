//! Call payload adapter over the existing bounded HTTP client and operator push credentials.
//! Ordinary message providers deliberately cannot encode this envelope (and PushService is a stub).
use super::*;
use crate::config::PushConfig;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use reqwest::{Client, StatusCode};
use tokio::sync::Mutex;

struct ApnsAuth {
    team: String,
    key_id: String,
    key: EncodingKey,
    cache: Mutex<Option<(String, i64)>>,
}
struct FcmAuth {
    email: String,
    key: EncodingKey,
    cache: Mutex<Option<(String, i64)>>,
}
pub struct CallHttpPushProvider {
    apns_client: Client,
    fcm_client: Client,
    apns_endpoint: String,
    fcm_endpoint: String,
    bundle: String,
    apns: Option<ApnsAuth>,
    fcm: Option<FcmAuth>,
    fixture_tokens: Option<(String, String)>,
}

impl CallHttpPushProvider {
    pub fn from_config(config: &PushConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(config.enabled, "Push infrastructure disabled");
        let mut apns = None;
        let mut fcm = None;
        let mut bundle = String::new();
        let mut fcm_endpoint = String::new();
        if config.apns.enabled {
            let c = &config.apns;
            bundle = c
                .bundle_id
                .clone()
                .ok_or_else(|| anyhow::anyhow!("APNs bundle missing"))?;
            anyhow::ensure!(
                !bundle.is_empty()
                    && bundle.len() <= 200
                    && bundle
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-'),
                "Invalid APNs bundle"
            );
            let key_bytes = std::fs::read(
                c.private_key_path
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("APNs key missing"))?,
            )?;
            apns = Some(ApnsAuth {
                team: c
                    .team_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("APNs team missing"))?,
                key_id: c
                    .key_id
                    .clone()
                    .ok_or_else(|| anyhow::anyhow!("APNs key ID missing"))?,
                key: EncodingKey::from_ec_pem(&key_bytes)?,
                cache: Mutex::new(None),
            });
        }
        if config.fcm.enabled {
            // No one-hour pasted-token production fallback. Use the same configured service account.
            let bytes = std::fs::read(
                config
                    .fcm
                    .service_account_path
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("FCM service account required"))?,
            )?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let field = |name: &str| {
                value[name]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("FCM credential field missing"))
            };
            let project = config
                .fcm
                .project_id
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(field("project_id")?);
            anyhow::ensure!(
                project.len() <= 128
                    && project
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
                "Invalid FCM project"
            );
            fcm_endpoint =
                format!("https://fcm.googleapis.com/v1/projects/{project}/messages:send");
            fcm = Some(FcmAuth {
                email: field("client_email")?.into(),
                key: EncodingKey::from_rsa_pem(field("private_key")?.as_bytes())?,
                cache: Mutex::new(None),
            });
        }
        anyhow::ensure!(
            apns.is_some() || fcm.is_some(),
            "No call push providers configured"
        );
        Ok(Self {
            apns_client: crate::push::provider::build_http_client(),
            fcm_client: crate::push::provider::build_http_client_with_proxy(
                config.fcm.https_proxy.as_deref(),
            ),
            apns_endpoint: if config.apns.use_sandbox {
                "https://api.sandbox.push.apple.com"
            } else {
                "https://api.push.apple.com"
            }
            .into(),
            fcm_endpoint,
            bundle,
            apns,
            fcm,
            fixture_tokens: None,
        })
    }

    /// Local HTTP provider acceptance only; endpoint is never request/config supplied in production.
    #[doc(hidden)]
    pub fn loopback_fixture(endpoint: &str) -> anyhow::Result<Self> {
        let url = url::Url::parse(endpoint)?;
        anyhow::ensure!(
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Only a loopback provider fixture is permitted"
        );
        Ok(Self {
            apns_client: crate::push::provider::build_http_client(),
            fcm_client: crate::push::provider::build_http_client(),
            apns_endpoint: endpoint.trim_end_matches('/').into(),
            fcm_endpoint: format!(
                "{}/v1/projects/fixture/messages:send",
                endpoint.trim_end_matches('/')
            ),
            bundle: "dev.privchat.fixture".into(),
            apns: None,
            fcm: None,
            fixture_tokens: Some(("fixture-apns-only".into(), "fixture-fcm-only".into())),
        })
    }

    async fn apns_token(&self) -> anyhow::Result<String> {
        if let Some(tokens) = &self.fixture_tokens {
            return Ok(tokens.0.clone());
        }
        let auth = self
            .apns
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("APNs unavailable"))?;
        let mut cache = auth.cache.lock().await;
        let time = now() / 1000;
        if let Some((token, issued)) = &*cache {
            if time - *issued < 3000 {
                return Ok(token.clone());
            }
        }
        let mut header = Header::new(Algorithm::ES256);
        header.kid = Some(auth.key_id.clone());
        let token = encode(&header, &json!({"iss":auth.team,"iat":time}), &auth.key)?;
        *cache = Some((token.clone(), time));
        Ok(token)
    }

    async fn fcm_token(&self) -> anyhow::Result<String> {
        if let Some(tokens) = &self.fixture_tokens {
            return Ok(tokens.1.clone());
        }
        let auth = self
            .fcm
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("FCM unavailable"))?;
        let mut cache = auth.cache.lock().await;
        let time = now() / 1000;
        if let Some((token, expiry)) = &*cache {
            if *expiry > time + 120 {
                return Ok(token.clone());
            }
        }
        let assertion = encode(
            &Header::new(Algorithm::RS256),
            &json!({"iss":auth.email,"scope":"https://www.googleapis.com/auth/firebase.messaging","aud":"https://oauth2.googleapis.com/token","iat":time,"exp":time+3600}),
            &auth.key,
        )?;
        let response = self
            .fcm_client
            .post("https://oauth2.googleapis.com/token")
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ])
            .send()
            .await?;
        anyhow::ensure!(response.status().is_success(), "FCM OAuth rejected");
        let value = bounded_json(response).await?;
        let token = value["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("FCM OAuth response invalid"))?
            .to_string();
        let ttl = value["expires_in"]
            .as_i64()
            .filter(|n| *n > 120)
            .ok_or_else(|| anyhow::anyhow!("FCM OAuth TTL invalid"))?;
        *cache = Some((token.clone(), time + ttl.min(3600)));
        Ok(token)
    }

    async fn deliver(&self, n: &CallPushNotification) -> anyhow::Result<CallPushResult> {
        if n.expires_at_ms <= now() {
            return Ok(CallPushResult::Rejected);
        }
        let response=match n.platform {
            CallPushPlatform::ApnsVoip=>{
                let token=if n.incoming { &n.token } else {
                    let Some(token)=n.ordinary_apns_token.as_ref().filter(|s|!s.is_empty()) else { return Ok(CallPushResult::NoControlToken); };token
                };
                let topic=if n.incoming { format!("{}.voip",self.bundle) } else { self.bundle.clone() };
                self.apns_client.post(format!("{}/3/device/{}",self.apns_endpoint,token))
                    .bearer_auth(self.apns_token().await?).header("apns-topic",topic)
                    .header("apns-push-type",if n.incoming{"voip"}else{"background"})
                    .header("apns-priority",if n.incoming{"10"}else{"5"}).header("apns-expiration","0")
                    .header("apns-id",n.delivery_id.to_string())
                    .json(&json!({"aps":{"content-available":1},"data":n.data})).send().await?
            }
            CallPushPlatform::Fcm=>self.fcm_client.post(&self.fcm_endpoint).bearer_auth(self.fcm_token().await?)
                .json(&json!({"message":{"token":n.token,"data":n.data,"android":{"priority":"HIGH","ttl":if n.incoming {format!("{}s",((n.expires_at_ms-now()).max(0)/1000).min(300))}else{"0s".into()}}}})).send().await?,
        };
        let status = response.status();
        if status.is_success() {
            return Ok(CallPushResult::Accepted);
        }
        if status == StatusCode::UNAUTHORIZED {
            if let Some(auth) = &self.fcm {
                *auth.cache.lock().await = None;
            }
        }
        if status.is_server_error()
            || status == StatusCode::TOO_MANY_REQUESTS
            || status == StatusCode::UNAUTHORIZED
            || status == StatusCode::FORBIDDEN
        {
            return Ok(CallPushResult::Retry);
        }
        let body = bounded_json(response).await.unwrap_or(Value::Null);
        let invalid = match n.platform {
            CallPushPlatform::ApnsVoip => {
                status == StatusCode::GONE
                    || (status == StatusCode::BAD_REQUEST && body["reason"] == "BadDeviceToken")
            }
            CallPushPlatform::Fcm => {
                status == StatusCode::NOT_FOUND
                    && body["error"]["details"]
                        .as_array()
                        .is_some_and(|a| a.iter().any(|d| d["errorCode"] == "UNREGISTERED"))
            }
        };
        Ok(if invalid {
            CallPushResult::InvalidToken
        } else {
            CallPushResult::Rejected
        })
    }
}

async fn bounded_json(mut response: reqwest::Response) -> anyhow::Result<Value> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= 16384,
            "Provider response too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

#[async_trait]
impl CallPushProvider for CallHttpPushProvider {
    fn supports(&self, p: CallPushPlatform) -> bool {
        self.fixture_tokens.is_some()
            || match p {
                CallPushPlatform::ApnsVoip => self.apns.is_some(),
                CallPushPlatform::Fcm => self.fcm.is_some(),
            }
    }
    async fn send(&self, n: &CallPushNotification) -> CallPushResult {
        self.deliver(n).await.unwrap_or(CallPushResult::Retry)
    }
}
