//! Server-owned component configuration. Public views are constructed by allowlist.
use super::*;
use crate::error::ServerError;

// Shared by every call creator and service update, across processes using this database.
pub const CONFIG_LOCK: i64 = 0x7076_6368_7274_6301;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RtcSettings {
    pub enabled: bool,
    pub livekit_url: String,
    pub api_key_env: String,
    pub api_secret_env: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateRtcSettings {
    pub expected_revision: String,
    pub settings: RtcSettings,
}

fn unavailable() -> ServerError {
    ServerError::ServiceUnavailable("Component configuration unavailable".into())
}
fn conflict(message: &str) -> ServerError {
    ServerError::Duplicate(message.into())
}
impl RtcSettings {
    fn local(config: &CallConfig) -> Self {
        Self {
            enabled: config.enabled,
            livekit_url: config.livekit_url.clone(),
            api_key_env: config.api_key_env.clone(),
            api_secret_env: config.api_secret_env.clone(),
        }
    }
    fn apply(&self, config: &mut CallConfig) {
        config.enabled = self.enabled;
        config.livekit_url = self.livekit_url.clone();
        config.api_key_env = self.api_key_env.clone();
        config.api_secret_env = self.api_secret_env.clone();
    }
    fn validate(&self, local: &CallConfig) -> Result<(), ServerError> {
        let invalid = || ServerError::Validation("Invalid RTC URL or environment reference".into());
        if !self.livekit_url.is_empty() && !valid_managed_url(&self.livekit_url) {
            return Err(invalid());
        }
        self.validate_references(local)
    }
    fn validate_references(&self, local: &CallConfig) -> Result<(), ServerError> {
        let invalid = || {
            ServerError::Validation("RTC environment reference is not allowed for this role".into())
        };
        // These role-specific names come only from deploy-owned config, never from settings.
        // Peer-only calls need no SFU. Full transport/TURN validation remains in CallConfig.
        let needs_livekit = self.enabled && !self.livekit_url.is_empty();
        for (name, allowed) in [
            (&self.api_key_env, &local.api_key_env),
            (&self.api_secret_env, &local.api_secret_env),
        ] {
            if name.is_empty() && !needs_livekit {
                continue;
            }
            if !valid_env_name(name) || name != allowed {
                return Err(invalid());
            }
            if needs_livekit && !std::env::var(name).is_ok_and(|v| !v.trim().is_empty()) {
                return Err(ServerError::Validation(
                    "RTC credential environment unavailable".into(),
                ));
            }
        }
        Ok(())
    }
}

impl CallService {
    /// Expire durable leases even without credentials; false forbids worker media dispatch.
    pub async fn sweep_for_worker(&self) -> RpcResult<bool> {
        match self.runtime().await {
            Ok(runtime) => {
                runtime.sweep().await?;
                Ok(true)
            }
            Err(_) => {
                tracing::warn!("Call runtime unavailable; expiring durable leases only");
                // Managed fields never change timeout policy. sweep only writes durable state
                // and pending effects; it never sends requests to this base service's SFU.
                self.sweep().await?;
                Ok(false)
            }
        }
    }

    async fn settings_in(
        &self,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(String, RtcSettings, bool), ServerError> {
        let row = sqlx::query(
            "SELECT revision,settings FROM privchat_component_config WHERE component='rtc'",
        )
        .fetch_optional(&mut **tx)
        .await
        .map_err(|_| unavailable())?;
        if let Some(row) = row {
            let settings =
                serde_json::from_value(row.get("settings")).map_err(|_| unavailable())?;
            Ok((row.get("revision"), settings, true))
        } else {
            let settings = RtcSettings::local(&self.config);
            // Local revisions also change when local policy changes. No credential material is hashed.
            let bytes = serde_json::to_vec(&self.config).map_err(|_| unavailable())?;
            Ok((
                format!("local-{}", hex::encode(Sha256::digest(bytes))),
                settings,
                false,
            ))
        }
    }
    pub async fn runtime_in(&self, tx: &mut Transaction<'_, Postgres>) -> RpcResult<Self> {
        sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
            .bind(CONFIG_LOCK)
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        let (_, settings, managed) = self.settings_in(tx).await.map_err(RpcError::from)?;
        if !managed {
            return Ok(self.clone());
        }
        // Enforce the deploy-owned boundary on old persisted rows too, before resolving any value.
        settings.validate_references(&self.config).map_err(|_| {
            RpcError::from_code(
                privchat_protocol::ErrorCode::ServiceUnavailable,
                "RTC runtime configuration unavailable".into(),
            )
        })?;
        let mut config = self.config.clone();
        settings.apply(&mut config);
        Self::from_environment(self.pool.clone(), config).map_err(|_| {
            RpcError::from_code(
                privchat_protocol::ErrorCode::ServiceUnavailable,
                "RTC runtime credentials unavailable".into(),
            )
        })
    }
    pub async fn runtime(&self) -> RpcResult<Self> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let runtime = self.runtime_in(&mut tx).await?;
        tx.commit().await.map_err(db)?;
        Ok(runtime)
    }
    fn public_view(&self, revision: &str, settings: &RtcSettings) -> Value {
        // Never serialize CallConfig/RtcSettings on a public route.
        json!({"schemaVersion":1,"revision":revision,"maxAgeSeconds":300,"components":{"rtc":{
            "schemaVersion":1,"enabled":settings.enabled,"livekitUrl":safe_url(&settings.livekit_url),
            "peerEnabled":settings.enabled && self.config.peer.enabled,
            "allowTurn":self.config.peer.allow_turn
        }}})
    }
    pub async fn component_view(&self, privileged: bool) -> Result<Value, ServerError> {
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        // Public discovery fails closed; service-key callers still need the stored CAS revision
        // to repair unavailable credentials. Storage errors continue to fail settings_in below.
        let runtime_available = self.runtime_in(&mut tx).await.is_ok();
        if !runtime_available && !privileged {
            return Err(unavailable());
        }
        let (revision, settings, _) = self.settings_in(&mut tx).await?;
        let mut view = self.public_view(&revision, &settings);
        if privileged {
            let mut settings = settings;
            settings.livekit_url = safe_url(&settings.livekit_url);
            for name in [&mut settings.api_key_env, &mut settings.api_secret_env] {
                if !valid_env_name(name) {
                    name.clear();
                }
            }
            view["settings"] = serde_json::to_value(settings).map_err(|_| unavailable())?;
            view["runtimeAvailable"] = json!(runtime_available);
        }
        tx.commit().await.map_err(|_| unavailable())?;
        Ok(view)
    }
    pub async fn update_rtc(&self, request: UpdateRtcSettings) -> Result<Value, ServerError> {
        let mut tx = self.pool.begin().await.map_err(|_| unavailable())?;
        let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
            .bind(CONFIG_LOCK)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| unavailable())?;
        if !locked {
            return Err(conflict(
                "RTC configuration busy; retry after reading current revision",
            ));
        }
        let (revision, _, _) = self.settings_in(&mut tx).await?;
        if request.expected_revision != revision {
            return Err(conflict("RTC configuration revision conflict"));
        }
        request.settings.validate(&self.config)?;
        let mut config = self.config.clone();
        request.settings.apply(&mut config);
        Self::from_environment(self.pool.clone(), config)
            .map_err(|_| ServerError::Validation("RTC runtime configuration unavailable".into()))?;
        // Cluster changes wait for media cleanup; disabling also drains terminal notifications/history.
        let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_calls WHERE state <> 'ended') OR EXISTS(SELECT 1 FROM privchat_call_effects e JOIN privchat_calls c USING(call_id) WHERE NOT e.completed AND (NOT $1 OR (e.kind IN ('delete_room','remove_participant') AND c.document->'call'->>'transport' = 'livekit')))")
            .bind(request.settings.enabled)
            .fetch_one(&mut *tx).await.map_err(|_| unavailable())?;
        if active {
            return Err(conflict(
                "RTC configuration cannot change while calls or required call effects are active",
            ));
        }
        let revision = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO privchat_component_config(component,revision,settings) VALUES('rtc',$1,$2) ON CONFLICT(component) DO UPDATE SET revision=EXCLUDED.revision,settings=EXCLUDED.settings")
            .bind(&revision).bind(serde_json::to_value(&request.settings).map_err(|_| unavailable())?)
            .execute(&mut *tx).await.map_err(|_| unavailable())?;
        let mut view = self.public_view(&revision, &request.settings);
        view["settings"] = serde_json::to_value(request.settings).map_err(|_| unavailable())?;
        view["runtimeAvailable"] = json!(true);
        tx.commit().await.map_err(|_| unavailable())?;
        Ok(view)
    }
}

fn safe_url(raw: &str) -> String {
    url::Url::parse(raw)
        .ok()
        .filter(|u| {
            matches!(u.scheme(), "ws" | "wss")
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
        })
        .map(|_| raw.to_owned())
        .unwrap_or_default()
}

fn valid_env_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
}

fn valid_managed_url(raw: &str) -> bool {
    // Match the application's HTTPS-equivalent policy without DNS-based loopback guesses.
    if raw.len() > 2048
        || raw.chars().any(|c| c.is_whitespace() || c.is_control())
        || raw.contains(['@', '?', '#', '\\'])
    {
        return false;
    }
    if !raw.starts_with("wss://")
        && !["ws://127.0.0.1:", "ws://localhost:", "ws://[::1]:"]
            .iter()
            .any(|prefix| raw.starts_with(prefix))
    {
        return false;
    }
    let Ok(url) = url::Url::parse(raw) else {
        return false;
    };
    let authority = raw.split_once("://").unwrap().1.split('/').next().unwrap();
    if authority.ends_with(':')
        || url.port() == Some(0)
        || !authority.is_ascii()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain(host)) => {
            host.len() <= 253
                && host.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                        && label
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                })
        }
        Some(_) => true,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_url_requires_wss_or_explicit_loopback_ws() {
        for raw in [
            "wss://rtc.example.com",
            "wss://rtc.example.com:443/rtc",
            "wss://[::1]:443",
            "ws://127.0.0.1:56780",
            "ws://localhost:80",
            "ws://[::1]:56780",
        ] {
            assert!(valid_managed_url(raw), "{raw}");
        }
        for raw in [
            "",
            "ws://external.example.com",
            "ws://192.168.1.1:80",
            "ws://127.0.0.1",
            "ws://localhost",
            "ws://127.1:80",
            "ws://2130706433:80",
            "ws://localhost.evil:80",
            "http://rtc.example.com",
            "wss://",
            "wss://rtc:0",
            "wss://rtc:99999",
            "wss://rtc:abc",
            "wss://rtc:",
            "wss://user:pass@rtc",
            "wss://@rtc",
            "wss://rtc?token=x",
            "wss://rtc/#fragment",
            "wss://rtc/\n",
            "wss://rtc/\u{7f}",
            "wss://rtc\\path",
            "wss://bad_host",
            "wss://rtc.",
        ] {
            assert!(!valid_managed_url(raw), "{raw:?}");
        }
    }

    #[test]
    fn disabled_settings_allow_blank_or_unresolved_but_not_malformed_fields() {
        let local = CallConfig {
            api_key_env: format!("UNSET_ALLOWED_KEY_{}", uuid::Uuid::new_v4().simple()),
            api_secret_env: format!("UNSET_ALLOWED_SECRET_{}", uuid::Uuid::new_v4().simple()),
            ..Default::default()
        };
        let mut settings = RtcSettings {
            enabled: false,
            livekit_url: String::new(),
            api_key_env: String::new(),
            api_secret_env: String::new(),
        };
        settings.validate(&local).unwrap();
        settings.enabled = true;
        settings.validate(&local).unwrap(); // Full CallConfig rejects this only if peer is also disabled.
        settings.enabled = false;
        settings.livekit_url = "wss://rtc.example.invalid".into();
        settings.api_key_env = local.api_key_env.clone();
        settings.api_secret_env = local.api_secret_env.clone();
        settings.validate(&local).unwrap();
        settings.enabled = true;
        assert!(settings.validate(&local).is_err());
        settings.enabled = false;
        settings.api_key_env = "not-an-env-name".into();
        assert!(settings.validate(&local).is_err());
        settings.api_key_env.clear();
        settings.livekit_url = "ws://external.example.invalid".into();
        assert!(settings.validate(&local).is_err());
        settings.livekit_url = "   ".into();
        assert!(settings.validate(&local).is_err());
    }

    #[test]
    fn enabled_peer_only_settings_do_not_require_livekit_dependencies() {
        let local = CallConfig {
            api_key_env: format!("UNSET_ALLOWED_KEY_{}", uuid::Uuid::new_v4().simple()),
            api_secret_env: format!("UNSET_ALLOWED_SECRET_{}", uuid::Uuid::new_v4().simple()),
            ..Default::default()
        };
        let mut settings = RtcSettings {
            enabled: true,
            livekit_url: String::new(),
            api_key_env: String::new(),
            api_secret_env: String::new(),
        };
        settings.validate(&local).unwrap();
        settings.api_key_env = local.api_key_env.clone();
        settings.api_secret_env = local.api_secret_env.clone();
        settings.validate(&local).unwrap();
        settings.livekit_url = "wss://rtc.example.invalid".into();
        assert!(
            settings.validate(&local).is_err(),
            "an enabled SFU must resolve its credential refs"
        );
        settings.api_key_env.clear();
        settings.api_secret_env.clear();
        assert!(
            settings.validate(&local).is_err(),
            "an enabled SFU must have credential refs"
        );
    }
}
