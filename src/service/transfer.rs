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

//! Channel Transfer foundations (spec `02-server/CHANNEL_TRANSFER_SPEC` v2.0).
//!
//! This module hosts the pure validators and the outbound HTTP relay client
//! that the wire ingress handler (Bite 2) and the `/api/service/transfer/send`
//! endpoint (Bite 3) build on top of. Nothing here touches msgtrans or axum
//! state — kept deliberately small and pure so the spec's validation rules
//! are unit-testable in isolation.

use std::time::Duration;

use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use reqwest::header::{HeaderName, HeaderValue};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::config::ChannelTransferConfig;

// =====================================================================
// Limits — quoted directly from spec §7 / §10
// =====================================================================

/// Maximum allowed `request_id` length (spec §7.1).
pub const MAX_TRANSFER_REQUEST_ID_LEN: usize = 64;

/// Maximum allowed `route` length (spec §6 — server-side string check).
pub const MAX_TRANSFER_ROUTE_LEN: usize = 256;

/// Maximum allowed `body` / `data` length, in raw bytes (spec §10).
pub const MAX_TRANSFER_BODY_BYTES: usize = 64 * 1024;

/// Outbound HTTP header carrying the application's master key (spec §13).
pub const SERVICE_KEY_HEADER: &str = "X-Service-Key";

// =====================================================================
// Validation
// =====================================================================

/// Errors returned by the pure validator helpers. Mapping to `ErrorCode`
/// numbers happens in the wire / HTTP layers — keeping the validator
/// transport-neutral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferValidationError {
    RequestIdEmpty,
    RequestIdTooLong { len: usize },
    RequestIdLowEntropy,
    RequestIdBadChar,
    RouteEmpty,
    RouteTooLong { len: usize },
    RouteLeadingSlash,
    RouteEmptySegment,
    RouteSegmentBadChar,
    RouteShape, // not exactly 3 segments
    BodyTooLarge { len: usize, limit: usize },
}

impl std::fmt::Display for TransferValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use TransferValidationError::*;
        match self {
            RequestIdEmpty => write!(f, "request_id is empty"),
            RequestIdTooLong { len } => {
                write!(f, "request_id length {len} exceeds {MAX_TRANSFER_REQUEST_ID_LEN}")
            }
            RequestIdLowEntropy => write!(
                f,
                "request_id entropy too low (need UUID v4 or >=16 random bytes)"
            ),
            RequestIdBadChar => write!(
                f,
                "request_id contains non-URL-safe character"
            ),
            RouteEmpty => write!(f, "route is empty"),
            RouteTooLong { len } => {
                write!(f, "route length {len} exceeds {MAX_TRANSFER_ROUTE_LEN}")
            }
            RouteLeadingSlash => write!(
                f,
                "route must not start with '/' (format: service/module/action)"
            ),
            RouteEmptySegment => write!(f, "route contains empty segment"),
            RouteSegmentBadChar => write!(
                f,
                "route segment contains non-lowercase / non-kebab-case character"
            ),
            RouteShape => write!(
                f,
                "route must have exactly 3 segments (service/module/action)"
            ),
            BodyTooLarge { len, limit } => {
                write!(f, "body size {len} bytes exceeds {limit}")
            }
        }
    }
}

impl std::error::Error for TransferValidationError {}

/// Validate `TransferRequest.request_id` per spec §7.1:
///
/// - non-empty
/// - length ≤ 64
/// - URL/log-safe ASCII characters
/// - sufficient entropy: either UUID v4 (with hyphens) or ≥16 alnum bytes
///   (Crockford-base32 / base32hex / base64url-no-pad / hex etc.)
///
/// Servers should reject with `10100 InvalidParams` on failure.
pub fn validate_transfer_request_id(request_id: &str) -> Result<(), TransferValidationError> {
    if request_id.is_empty() {
        return Err(TransferValidationError::RequestIdEmpty);
    }
    if request_id.len() > MAX_TRANSFER_REQUEST_ID_LEN {
        return Err(TransferValidationError::RequestIdTooLong {
            len: request_id.len(),
        });
    }
    if !request_id
        .bytes()
        .all(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
    {
        return Err(TransferValidationError::RequestIdBadChar);
    }

    // UUID v4 form: 8-4-4-4-12 hex with hyphens, length 36, version nibble = '4'
    if request_id.len() == 36 && uuid_v4_shape(request_id) {
        return Ok(());
    }

    // Otherwise: require ≥16 ASCII-alnum bytes (≥80 bits entropy)
    let alnum_count = request_id
        .bytes()
        .filter(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9'))
        .count();
    if alnum_count < 16 {
        return Err(TransferValidationError::RequestIdLowEntropy);
    }
    Ok(())
}

fn uuid_v4_shape(s: &str) -> bool {
    let bytes = s.as_bytes();
    let hyphens_at = [8usize, 13, 18, 23];
    for (i, b) in bytes.iter().enumerate() {
        let is_hy = hyphens_at.contains(&i);
        if is_hy {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    // version nibble is the first char of the third group (index 14)
    bytes.get(14).copied() == Some(b'4')
}

/// Validate `TransferRequest.route` for *server-side string format* only
/// (spec §6). Format is `service/module/action`:
///
/// - non-empty, length ≤ 256
/// - does not start with '/'
/// - exactly three slash-delimited segments, none empty
/// - each segment is `[a-z0-9-]+`, must not start or end with '-'
///
/// Server does **not** validate the prefix against any service registry —
/// that is the application's job (dispatch spec §10.1).
pub fn validate_transfer_route(route: &str) -> Result<(), TransferValidationError> {
    if route.is_empty() {
        return Err(TransferValidationError::RouteEmpty);
    }
    if route.len() > MAX_TRANSFER_ROUTE_LEN {
        return Err(TransferValidationError::RouteTooLong { len: route.len() });
    }
    if route.starts_with('/') {
        return Err(TransferValidationError::RouteLeadingSlash);
    }
    let segments: Vec<&str> = route.split('/').collect();
    if segments.len() != 3 {
        return Err(TransferValidationError::RouteShape);
    }
    for seg in &segments {
        if seg.is_empty() {
            return Err(TransferValidationError::RouteEmptySegment);
        }
        if !is_kebab_lower(seg) {
            return Err(TransferValidationError::RouteSegmentBadChar);
        }
    }
    Ok(())
}

fn is_kebab_lower(seg: &str) -> bool {
    let bytes = seg.as_bytes();
    if bytes.first().copied() == Some(b'-') || bytes.last().copied() == Some(b'-') {
        return false;
    }
    bytes
        .iter()
        .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

/// Validate `TransferRequest.body` / `TransferResponse.data` size (spec §10).
pub fn validate_transfer_body_size(body: &[u8]) -> Result<(), TransferValidationError> {
    if body.len() > MAX_TRANSFER_BODY_BYTES {
        return Err(TransferValidationError::BodyTooLarge {
            len: body.len(),
            limit: MAX_TRANSFER_BODY_BYTES,
        });
    }
    Ok(())
}

// =====================================================================
// Outbound relay: server → application
//   POST {application_url}/service/privchat/transfer
//   X-Service-Key: <application_master_key>
// =====================================================================

/// Server-side payload for `POST /service/privchat/transfer` (spec §5.1).
///
/// `body` and `data` cross HTTP as base64; this struct uses `Vec<u8>` and
/// serializes via the `base64_bytes` adapter so call-sites stay
/// transport-agnostic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardTransferRequest {
    pub internal_request_id: String,
    pub client_request_id: String,
    pub channel_id: u64,
    pub room_id: u64,
    pub user_id: u64,
    pub route: String,
    #[serde(with = "base64_bytes")]
    pub body: Vec<u8>,
    pub trace_id: String,
}

/// Server-side payload of the application's response on the same endpoint
/// (spec §5.1, flat shape — *not* `ApiEnvelope`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardTransferResponse {
    pub client_request_id: String,
    pub channel_id: u64,
    pub code: i32,
    pub message: String,
    #[serde(with = "base64_bytes_opt", default)]
    pub data: Option<Vec<u8>>,
}

mod base64_bytes {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Vec<u8>, ser: S) -> Result<S::Ok, S::Error> {
        ser.serialize_str(&BASE64_STD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(de: D) -> Result<Vec<u8>, D::Error> {
        use serde::Deserialize as _;
        let s = String::deserialize(de)?;
        if s.is_empty() {
            return Ok(Vec::new());
        }
        BASE64_STD.decode(&s).map_err(serde::de::Error::custom)
    }
}

mod base64_bytes_opt {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(opt: &Option<Vec<u8>>, ser: S) -> Result<S::Ok, S::Error> {
        match opt {
            Some(bytes) => ser.serialize_str(&BASE64_STD.encode(bytes)),
            None => ser.serialize_str(""),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        de: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        use serde::Deserialize as _;
        // Tolerate missing field, JSON null, and empty string — all decode to None.
        let opt: Option<String> = Option::deserialize(de)?;
        let s = match opt {
            None => return Ok(None),
            Some(s) if s.is_empty() => return Ok(None),
            Some(s) => s,
        };
        BASE64_STD
            .decode(&s)
            .map(Some)
            .map_err(serde::de::Error::custom)
    }
}

/// Outbound HTTP client for `POST /service/privchat/transfer`.
///
/// Owns nothing application-specific: takes a [`ChannelTransferConfig`] up
/// front, builds a reqwest client with the configured timeout, and exposes
/// a single [`forward`](Self::forward) method for the wire ingress handler
/// (Bite 2) to call.
pub struct TransferRelayClient {
    http: reqwest::Client,
    endpoint: String,
    master_key: HeaderValue,
}

/// Why a relay attempt failed. Mapped to `protocol::ErrorCode` by the caller
/// (typically the wire ingress handler).
#[derive(Debug)]
pub enum RelayError {
    /// Reached app, app responded but HTTP status was non-2xx — the body
    /// (already attempted as `ForwardTransferResponse`) is included if
    /// parseable.
    AppHttpError {
        status: StatusCode,
        body: Option<ForwardTransferResponse>,
        raw: String,
    },
    /// HTTP 2xx but body did not parse as `ForwardTransferResponse`.
    AppMalformed { raw: String, error: String },
    /// reqwest reported a timeout (request took longer than configured).
    Timeout,
    /// reqwest reported a transport-level failure (connect refused, DNS, etc.).
    Transport(String),
    /// Misconfigured at construction time (bad URL, bad header value).
    Config(String),
}

impl std::fmt::Display for RelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelayError::AppHttpError { status, .. } => {
                write!(f, "application returned HTTP {status}")
            }
            RelayError::AppMalformed { error, .. } => {
                write!(f, "application response malformed: {error}")
            }
            RelayError::Timeout => write!(f, "application call timed out"),
            RelayError::Transport(msg) => write!(f, "transport error: {msg}"),
            RelayError::Config(msg) => write!(f, "relay misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for RelayError {}

impl TransferRelayClient {
    /// Build a relay client from configuration. Returns `Err` on bad URL or
    /// unrepresentable header value.
    pub fn new(cfg: &ChannelTransferConfig) -> Result<Self, RelayError> {
        let endpoint = format!(
            "{}/service/privchat/transfer",
            cfg.application_url.trim_end_matches('/')
        );
        let master_key = HeaderValue::from_str(&cfg.application_master_key).map_err(|e| {
            RelayError::Config(format!("application_master_key not header-safe: {e}"))
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| RelayError::Config(format!("reqwest builder: {e}")))?;
        Ok(Self {
            http,
            endpoint,
            master_key,
        })
    }

    /// Endpoint that this client posts to (useful for logs / tests).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Send a forwarded TransferRequest to the application and return its
    /// flat-shaped response.
    pub async fn forward(
        &self,
        req: &ForwardTransferRequest,
    ) -> Result<ForwardTransferResponse, RelayError> {
        let header_name: HeaderName = SERVICE_KEY_HEADER.parse().expect("static header name");
        let response = self
            .http
            .post(&self.endpoint)
            .header(header_name, self.master_key.clone())
            .json(req)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    RelayError::Timeout
                } else {
                    RelayError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| RelayError::Transport(format!("read body: {e}")))?;

        if !status.is_success() {
            let body = serde_json::from_str::<ForwardTransferResponse>(&raw).ok();
            return Err(RelayError::AppHttpError { status, body, raw });
        }

        serde_json::from_str::<ForwardTransferResponse>(&raw).map_err(|e| RelayError::AppMalformed {
            raw,
            error: e.to_string(),
        })
    }
}

