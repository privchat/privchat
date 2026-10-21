//! Optional call control plane. Secrets are resolved from environment, never serialized.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CallConfig {
    pub peer: CallPeerSettings,
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
            peer: CallPeerSettings::default(),
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
        self.peer.validate()?;
        if !self.livekit_url.is_empty() || !self.peer.enabled {
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
        }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CallPeerSettings {
    pub enabled: bool,
    pub allow_turn: bool,
    pub stun_urls: Vec<String>,
    pub turn_urls: Vec<String>,
    pub turn_secret_env: String,
    pub credential_ttl_seconds: u32,
    pub journal_ttl_seconds: u32,
}
impl Default for CallPeerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            allow_turn: false,
            stun_urls: Vec::new(),
            turn_urls: Vec::new(),
            turn_secret_env: "PRIVCHAT_TURN_SECRET".into(),
            credential_ttl_seconds: 600,
            journal_ttl_seconds: 300,
        }
    }
}
impl CallPeerSettings {
    fn validate(&self) -> anyhow::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.allow_turn {
            anyhow::ensure!(
                !self.turn_urls.is_empty(),
                "Enabled TURN requires fallback URLs"
            );
        }
        let turn_urls = if self.allow_turn {
            self.turn_urls.as_slice()
        } else {
            &[]
        };
        anyhow::ensure!(
            self.stun_urls.len() + turn_urls.len() <= 16,
            "Too many ICE URLs"
        );
        for (urls, schemes) in [
            (self.stun_urls.as_slice(), &["stun", "stuns"][..]),
            (turn_urls, &["turn", "turns"][..]),
        ] {
            for raw in urls {
                let (scheme, rest) = raw
                    .split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("Invalid ICE URL"))?;
                anyhow::ensure!(
                    schemes.contains(&scheme)
                        && !rest.is_empty()
                        && raw.len() <= 512
                        && !rest.contains('@')
                        && !rest.contains('#')
                        && !rest.contains('/')
                        && !raw.chars().any(char::is_whitespace),
                    "Invalid ICE URL"
                );
                if let Some((_, query)) = rest.split_once('?') {
                    anyhow::ensure!(
                        schemes.contains(&"turn")
                            && matches!(query, "transport=udp" | "transport=tcp"),
                        "Invalid ICE transport"
                    );
                }
            }
        }
        if self.allow_turn {
            anyhow::ensure!(
                (60..=3600).contains(&self.credential_ttl_seconds),
                "TURN credential TTL must be 60..3600 seconds"
            );
        }
        anyhow::ensure!(
            (60..=900).contains(&self.journal_ttl_seconds),
            "Peer journal TTL must be 60..900 seconds"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_turn_defaults_false_and_stun_is_optional() {
        let config: CallConfig = toml::from_str("enabled=true\n[peer]\nenabled=true\n").unwrap();
        assert!(!config.peer.allow_turn);
        config.validate().unwrap();
        let mut stun = config.clone();
        stun.peer.stun_urls = vec!["stun:stun.example.invalid:3478".into()];
        stun.validate().unwrap();
        stun.peer.stun_urls = vec!["turn:relay.example.invalid:3478".into()];
        assert!(
            stun.validate().is_err(),
            "STUN slot must never leak a TURN URL"
        );
    }

    #[test]
    fn allow_turn_requires_urls_and_valid_ttl_only_when_enabled() {
        let mut config = CallConfig {
            enabled: true,
            ..Default::default()
        };
        config.peer.enabled = true;
        config.peer.turn_urls = vec!["not-used-while-disabled".into()];
        config.peer.credential_ttl_seconds = 0;
        config.validate().unwrap();
        config.peer.allow_turn = true;
        assert!(config.validate().is_err());
        config.peer.turn_urls.clear();
        config.peer.credential_ttl_seconds = 600;
        assert!(config.validate().is_err());
        config.peer.turn_urls = vec!["turn:relay.example.invalid:3478?transport=udp".into()];
        config.validate().unwrap();
    }

    #[test]
    fn generated_config_keeps_turn_opt_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        crate::config::generate_config_with_tls(path.to_str().unwrap()).unwrap();
        let mut config = crate::config::ServerConfig::from_toml_file(&path)
            .unwrap()
            .call;
        assert!(!config.peer.allow_turn);
        assert!(config.peer.turn_urls.is_empty());
        config.enabled = true;
        config.peer.enabled = true;
        config.validate().unwrap();
    }

    #[tokio::test]
    async fn startup_without_turn_secret_and_enabled_turn_fails_closed() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/unused_call_config_test")
            .unwrap();
        let mut config = CallConfig {
            enabled: true,
            ..Default::default()
        };
        config.peer.enabled = true;
        config.peer.turn_secret_env =
            format!("UNSET_CALL_TURN_TEST_{}", uuid::Uuid::new_v4().simple());
        assert!(
            super::super::call_service::CallService::from_environment(pool.clone(), config.clone())
                .unwrap()
                .capabilities()
                .peer_enabled
        );
        config.peer.allow_turn = true;
        config.peer.turn_urls = vec!["turn:relay.example.invalid:3478".into()];
        assert!(super::super::call_service::CallService::from_environment(
            pool.clone(),
            config.clone()
        )
        .is_err());
        assert!(super::super::call_service::CallService::with_peer_secret(
            pool.clone(),
            config.clone(),
            None,
            Some("short".into())
        )
        .is_err());
        assert!(super::super::call_service::CallService::with_peer_secret(
            pool,
            config,
            None,
            Some("isolated-test-secret".into())
        )
        .is_ok());
    }
}
