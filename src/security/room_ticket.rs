// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Room subscribe ticket sign / verify.
//!
//! Spec: `02-server/ROOM_CHANNEL_SPEC` §4.
//!
//! Ticket is a JWT with HMAC-SHA256. Claims (spec §4.3):
//!
//! | claim   | type   | meaning                                                |
//! |---------|--------|--------------------------------------------------------|
//! | `sub`   | string | user_id                                                |
//! | `did`   | string | device_id (binds the ticket to a device, stable across reconnects) |
//! | `cid`   | u64    | channel_id (target Room)                               |
//! | `ct`    | u8     | channel_type — must be `2` (Room)                      |
//! | `scope` | string | fixed `"subscribe"`                                    |
//! | `exp`   | u64    | expiry (Unix seconds; recommended 5 min)               |
//!
//! Issued by business APIs (e.g. `POST /api/app/rooms/{room_id}/join`),
//! verified at the wire `SubscribeRequest` ingress (gateway). The
//! signing key is shared between server and business API via the
//! same `[room_ticket]` config (or — in cross-process deploys —
//! the same secret installed on both sides).

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{
    decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};

use crate::config::RoomTicketConfig;

/// Channel type sentinel — Room is the only type allowed in the `ct` claim.
pub const CT_ROOM: u8 = 2;

/// Scope sentinel — only "subscribe" is recognised in v1.
pub const SCOPE_SUBSCRIBE: &str = "subscribe";

/// JWT claims for a Room subscribe ticket (spec §4.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicketClaims {
    pub sub: String,
    pub did: String,
    pub cid: u64,
    pub ct: u8,
    pub scope: String,
    pub exp: u64,
}

impl RoomTicketClaims {
    /// Construct fresh `subscribe` claims with `ttl_secs` from now.
    pub fn for_subscribe(
        user_id: u64,
        device_id: impl Into<String>,
        channel_id: u64,
        ttl_secs: u64,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            sub: user_id.to_string(),
            did: device_id.into(),
            cid: channel_id,
            ct: CT_ROOM,
            scope: SCOPE_SUBSCRIBE.to_string(),
            exp: now.saturating_add(ttl_secs),
        }
    }
}

/// Verification failure cause; maps to SubscribeResponse reason_code at the
/// handler. We split the cases for ops/debug visibility — `Display` is what
/// the warn log shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// `param` empty or missing; user did not present a ticket.
    Missing,
    /// JWT header could not be parsed (bad base64, malformed, etc.).
    MalformedHeader,
    /// JWT signature did not match — invalid secret or tampering.
    InvalidSignature,
    /// `exp` is in the past (taking leeway into account).
    Expired,
    /// `scope` is not `"subscribe"`.
    BadScope,
    /// `ct` is not `2` (Room).
    BadChannelType,
    /// `cid` claim does not match the requested `channel_id`.
    ChannelMismatch,
    /// `did` claim does not match the connection's `device_id`.
    DeviceMismatch,
    /// `kid` in the header was not found in config.
    UnknownKid,
    /// Anything else (claims malformed beyond a single recognisable cause).
    Other,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            Self::Missing => "missing",
            Self::MalformedHeader => "malformed_header",
            Self::InvalidSignature => "invalid_signature",
            Self::Expired => "expired",
            Self::BadScope => "bad_scope",
            Self::BadChannelType => "bad_channel_type",
            Self::ChannelMismatch => "channel_mismatch",
            Self::DeviceMismatch => "device_mismatch",
            Self::UnknownKid => "unknown_kid",
            Self::Other => "other",
        };
        f.write_str(label)
    }
}

/// Sign a ticket with the given key id (must exist in `cfg`).
/// Returns the compact-serialized JWT string.
///
/// Errors: only when the `kid` is not configured. Encoding itself does not
/// fail for valid HS256 inputs.
pub fn sign(cfg: &RoomTicketConfig, kid: &str, claims: &RoomTicketClaims) -> Result<String, &'static str> {
    let secret = cfg.resolve_secret(Some(kid)).ok_or("unknown kid")?;
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid.to_string());
    let key = EncodingKey::from_secret(secret.as_bytes());
    encode(&header, claims, &key).map_err(|_| "encode failed")
}

/// Convenience: sign with config's `default_kid`.
pub fn sign_default(cfg: &RoomTicketConfig, claims: &RoomTicketClaims) -> Result<String, &'static str> {
    let kid = cfg.default_kid.clone();
    sign(cfg, &kid, claims)
}

/// Verify a ticket against the spec §4.6 checklist.
///
/// Returns the parsed claims on success — caller may want to log `sub`.
pub fn verify(
    cfg: &RoomTicketConfig,
    token: &str,
    expected_channel_id: u64,
    expected_device_id: &str,
) -> Result<RoomTicketClaims, VerifyError> {
    if token.is_empty() {
        return Err(VerifyError::Missing);
    }

    // 1) Inspect header for `kid` to pick the right secret.
    let header = decode_header(token).map_err(|_| VerifyError::MalformedHeader)?;
    if header.alg != Algorithm::HS256 {
        return Err(VerifyError::MalformedHeader);
    }
    let secret = cfg
        .resolve_secret(header.kid.as_deref())
        .ok_or(VerifyError::UnknownKid)?;

    // 2) Decode + signature check + exp check (jsonwebtoken handles `exp` itself).
    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = cfg.leeway_secs;
    validation.set_required_spec_claims(&["exp"]);
    // Disable audience / issuer enforcement — spec §4.3 doesn't mandate them.
    validation.validate_aud = false;
    let data = decode::<RoomTicketClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => VerifyError::Expired,
        jsonwebtoken::errors::ErrorKind::InvalidSignature => VerifyError::InvalidSignature,
        jsonwebtoken::errors::ErrorKind::InvalidToken
        | jsonwebtoken::errors::ErrorKind::InvalidIssuer
        | jsonwebtoken::errors::ErrorKind::InvalidAudience
        | jsonwebtoken::errors::ErrorKind::InvalidSubject
        | jsonwebtoken::errors::ErrorKind::Base64(_)
        | jsonwebtoken::errors::ErrorKind::Json(_)
        | jsonwebtoken::errors::ErrorKind::Utf8(_) => VerifyError::Other,
        _ => VerifyError::Other,
    })?;
    let claims = data.claims;

    // 3) Spec §4.6 claim checks beyond what jsonwebtoken did.
    if claims.scope != SCOPE_SUBSCRIBE {
        return Err(VerifyError::BadScope);
    }
    if claims.ct != CT_ROOM {
        return Err(VerifyError::BadChannelType);
    }
    if claims.cid != expected_channel_id {
        return Err(VerifyError::ChannelMismatch);
    }
    if claims.did != expected_device_id {
        return Err(VerifyError::DeviceMismatch);
    }

    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn cfg_single(secret: &str) -> RoomTicketConfig {
        RoomTicketConfig {
            secret: Some(secret.to_string()),
            keys: HashMap::new(),
            default_kid: "v1".to_string(),
            leeway_secs: 30,
        }
    }

    fn cfg_keys(default_kid: &str, pairs: &[(&str, &str)]) -> RoomTicketConfig {
        let mut keys = HashMap::new();
        for (k, v) in pairs {
            keys.insert((*k).to_string(), (*v).to_string());
        }
        RoomTicketConfig {
            secret: None,
            keys,
            default_kid: default_kid.to_string(),
            leeway_secs: 30,
        }
    }

    #[test]
    fn sign_verify_roundtrip_single_key() {
        let cfg = cfg_single("super-secret-32-bytes-or-more!!!");
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        let token = sign_default(&cfg, &claims).expect("sign");
        let ok = verify(&cfg, &token, 9001, "dev-a").expect("verify");
        assert_eq!(ok.sub, "100");
        assert_eq!(ok.cid, 9001);
        assert_eq!(ok.did, "dev-a");
    }

    #[test]
    fn channel_mismatch_rejected() {
        let cfg = cfg_single("s");
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        let token = sign_default(&cfg, &claims).unwrap();
        let err = verify(&cfg, &token, 9999, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::ChannelMismatch);
    }

    #[test]
    fn device_mismatch_rejected() {
        let cfg = cfg_single("s");
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        let token = sign_default(&cfg, &claims).unwrap();
        let err = verify(&cfg, &token, 9001, "dev-b").unwrap_err();
        assert_eq!(err, VerifyError::DeviceMismatch);
    }

    #[test]
    fn empty_token_is_missing() {
        let cfg = cfg_single("s");
        let err = verify(&cfg, "", 1, "d").unwrap_err();
        assert_eq!(err, VerifyError::Missing);
    }

    #[test]
    fn malformed_token_is_malformed_header() {
        let cfg = cfg_single("s");
        let err = verify(&cfg, "not-a-jwt", 1, "d").unwrap_err();
        assert_eq!(err, VerifyError::MalformedHeader);
    }

    #[test]
    fn wrong_secret_invalid_signature() {
        let cfg_sign = cfg_single("right-secret");
        let cfg_verify = cfg_single("wrong-secret");
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        let token = sign_default(&cfg_sign, &claims).unwrap();
        let err = verify(&cfg_verify, &token, 9001, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::InvalidSignature);
    }

    #[test]
    fn expired_token_rejected() {
        let cfg = cfg_single("s");
        let mut claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        // backdate the exp to before now - leeway
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        claims.exp = now.saturating_sub(120);
        let token = sign_default(&cfg, &claims).unwrap();
        let err = verify(&cfg, &token, 9001, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::Expired);
    }

    #[test]
    fn wrong_scope_rejected() {
        let cfg = cfg_single("s");
        let mut claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        claims.scope = "publish".to_string();
        let token = sign_default(&cfg, &claims).unwrap();
        let err = verify(&cfg, &token, 9001, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::BadScope);
    }

    #[test]
    fn wrong_ct_rejected() {
        let cfg = cfg_single("s");
        let mut claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        claims.ct = 1; // Group, not Room
        let token = sign_default(&cfg, &claims).unwrap();
        let err = verify(&cfg, &token, 9001, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::BadChannelType);
    }

    #[test]
    fn key_rotation_via_kid() {
        let cfg = cfg_keys("v2", &[("v1", "old"), ("v2", "new")]);
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        // sign with old kid v1; verify still works because v1 still in keys
        let token_v1 = sign(&cfg, "v1", &claims).unwrap();
        verify(&cfg, &token_v1, 9001, "dev-a").expect("v1 still valid during rotation");
        // sign with v2 (default)
        let token_v2 = sign_default(&cfg, &claims).unwrap();
        verify(&cfg, &token_v2, 9001, "dev-a").expect("v2 ok");
    }

    #[test]
    fn unknown_kid_rejected() {
        let cfg = cfg_keys("v1", &[("v1", "k1")]);
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        // simulate a ticket signed with v0 (not in cfg).keys
        let other_cfg = cfg_keys("v0", &[("v0", "k0")]);
        let token = sign(&other_cfg, "v0", &claims).unwrap();
        let err = verify(&cfg, &token, 9001, "dev-a").unwrap_err();
        assert_eq!(err, VerifyError::UnknownKid);
    }
}
