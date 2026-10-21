//! Optional call control plane. Secrets are resolved from environment, never serialized.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CallConfig {
    pub enabled: bool,
    pub livekit_url: String,
    pub api_key_env: String,
    pub api_secret_env: String,
    pub token_ttl_seconds: u32,
    pub ringing_timeout_seconds: u32,
    pub heartbeat_lease_seconds: u32,
    pub audio_participant_limit: u32,
    pub video_participant_limit: u32,
}
impl Default for CallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            livekit_url: String::new(),
            api_key_env: "PRIVCHAT_LIVEKIT_API_KEY".into(),
            api_secret_env: "PRIVCHAT_LIVEKIT_API_SECRET".into(),
            token_ttl_seconds: 60,
            ringing_timeout_seconds: 45,
            heartbeat_lease_seconds: 60,
            audio_participant_limit: 16,
            video_participant_limit: 9,
        }
    }
}
impl CallConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let url = url::Url::parse(&self.livekit_url)?;
        anyhow::ensure!(
            matches!(url.scheme(), "ws" | "wss") && url.host_str().is_some(),
            "call.livekit_url must be ws(s)"
        );
        anyhow::ensure!(
            url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "call URL must not contain credentials, query or fragment"
        );
        anyhow::ensure!(
            (10..=300).contains(&self.token_ttl_seconds),
            "call token TTL must be 10..300 seconds"
        );
        anyhow::ensure!(
            (5..=300).contains(&self.ringing_timeout_seconds),
            "call ringing timeout must be 5..300 seconds"
        );
        anyhow::ensure!(
            (10..=300).contains(&self.heartbeat_lease_seconds),
            "call heartbeat lease must be 10..300 seconds"
        );
        anyhow::ensure!(
            (2..=128).contains(&self.audio_participant_limit)
                && (2..=128).contains(&self.video_participant_limit),
            "call participant limits must be 2..128"
        );
        Ok(())
    }
}
