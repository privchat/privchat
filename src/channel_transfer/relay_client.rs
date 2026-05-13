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

//! Outbound HTTP client: server → application
//!
//! `POST {application_url}/service/privchat/transfer/dispatch`
//! with `X-Service-Key: <application_master_key>` (spec §5.1 / §13.1).
//!
//! See module-level docs in [`super`] for the broader scope and the
//! "not a business service" rule from spec §1.4.

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::StatusCode;

use crate::channel_transfer::types::{
    ForwardTransferRequest, ForwardTransferResponse, SERVICE_KEY_HEADER,
};
use crate::config::ChannelTransferConfig;

/// Outbound HTTP client for `POST /service/privchat/transfer/dispatch`.
///
/// Owns nothing application-specific: takes a [`ChannelTransferConfig`] up
/// front, builds a reqwest client with the configured timeout, and exposes
/// a single [`forward`](Self::forward) method for the wire ingress handler
/// (Bite 2) to call.
pub struct ChannelTransferRelayClient {
    http: reqwest::Client,
    endpoint: String,
    master_key: HeaderValue,
}

/// Why a relay attempt failed. Mapped to `protocol::ErrorCode` by the caller
/// (typically the wire ingress handler).
#[derive(Debug)]
pub enum ChannelTransferRelayError {
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

impl std::fmt::Display for ChannelTransferRelayError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelTransferRelayError::AppHttpError { status, .. } => {
                write!(f, "application returned HTTP {status}")
            }
            ChannelTransferRelayError::AppMalformed { error, .. } => {
                write!(f, "application response malformed: {error}")
            }
            ChannelTransferRelayError::Timeout => write!(f, "application call timed out"),
            ChannelTransferRelayError::Transport(msg) => write!(f, "transport error: {msg}"),
            ChannelTransferRelayError::Config(msg) => write!(f, "relay misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for ChannelTransferRelayError {}

impl ChannelTransferRelayClient {
    /// Build a relay client from configuration. Returns `Err` on bad URL or
    /// unrepresentable header value.
    pub fn new(cfg: &ChannelTransferConfig) -> Result<Self, ChannelTransferRelayError> {
        // Spec 02-server/CHANNEL_TRANSFER_SPEC §5.1 + 07-application
        // /CHANNEL_TRANSFER_DISPATCH_SPEC §1.4: application's single dispatch
        // entry. Server never targets per-business endpoints — always /dispatch.
        let endpoint = format!(
            "{}/service/privchat/transfer/dispatch",
            cfg.application_url.trim_end_matches('/')
        );
        let master_key = HeaderValue::from_str(&cfg.application_master_key).map_err(|e| {
            ChannelTransferRelayError::Config(format!(
                "application_master_key not header-safe: {e}"
            ))
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| {
                ChannelTransferRelayError::Config(format!("reqwest builder: {e}"))
            })?;
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
    ) -> Result<ForwardTransferResponse, ChannelTransferRelayError> {
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
                    ChannelTransferRelayError::Timeout
                } else {
                    ChannelTransferRelayError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ChannelTransferRelayError::Transport(format!("read body: {e}")))?;

        if !status.is_success() {
            let body = parse_transfer_response(&raw);
            return Err(ChannelTransferRelayError::AppHttpError { status, body, raw });
        }

        parse_transfer_response(&raw).ok_or_else(|| {
            ChannelTransferRelayError::AppMalformed {
                raw: raw.clone(),
                error: format!(
                    "body could not be parsed as flat ForwardTransferResponse \
                     nor as ApiEnvelope-wrapped variant: {raw}"
                ),
            }
        })
    }
}

/// Try parsing the raw body as either:
///   1. flat `ForwardTransferResponse` (spec §5.1 shape), or
///   2. ApiEnvelope-wrapped `{"code":0, "message":"OK", "data": ForwardTransferResponse}`
///      — what the application currently emits via Neton's automatic envelope
///      wrap. Removing the wrap framework-side is a much larger change; this
///      tolerant parse keeps the wire boundary compatible without rocking
///      that boat.
fn parse_transfer_response(raw: &str) -> Option<ForwardTransferResponse> {
    if let Ok(r) = serde_json::from_str::<ForwardTransferResponse>(raw) {
        return Some(r);
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let inner = value.get("data")?;
    serde_json::from_value::<ForwardTransferResponse>(inner.clone()).ok()
}
