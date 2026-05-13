// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Outbound HTTP client for `POST /service/privchat/service-account/event`
//! (spec `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §5).
//!
//! Mirrors [`crate::channel_transfer::relay_client::ChannelTransferRelayClient`]
//! in shape — same timeout / `X-Service-Key` semantics — but the body is the
//! flat `ServiceAccountEvent` JSON, the response is a flat `ServiceAccountEventAck`,
//! and failures are warn-logged at the call-site (best-effort, no retry).

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::StatusCode;

use crate::channel_transfer::types::SERVICE_KEY_HEADER;
use crate::config::ServiceAccountEventConfig;
use crate::service_account_event::types::{
    ServiceAccountEvent, ServiceAccountEventAck, SERVICE_ACCOUNT_EVENT_PATH,
};

/// Why a service-account event delivery failed.
#[derive(Debug)]
pub enum ServiceAccountEventError {
    /// App reachable but returned non-2xx; the parsed ack (if any) is included.
    AppHttpError {
        status: StatusCode,
        body: Option<ServiceAccountEventAck>,
        raw: String,
    },
    /// HTTP 2xx but body did not parse as `ServiceAccountEventAck`.
    AppMalformed { raw: String, error: String },
    /// reqwest reported a timeout.
    Timeout,
    /// Transport-level failure (connect refused, DNS, etc.).
    Transport(String),
    /// Misconfigured at construction time (bad URL, bad header value).
    Config(String),
}

impl std::fmt::Display for ServiceAccountEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AppHttpError { status, .. } => {
                write!(f, "application returned HTTP {status}")
            }
            Self::AppMalformed { error, .. } => {
                write!(f, "application response malformed: {error}")
            }
            Self::Timeout => write!(f, "application call timed out"),
            Self::Transport(msg) => write!(f, "transport error: {msg}"),
            Self::Config(msg) => write!(f, "client misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for ServiceAccountEventError {}

/// HTTP caller for the service-account event webhook.
pub struct ServiceAccountEventClient {
    http: reqwest::Client,
    endpoint: String,
    master_key: HeaderValue,
}

impl ServiceAccountEventClient {
    /// Build a client from configuration.
    pub fn new(cfg: &ServiceAccountEventConfig) -> Result<Self, ServiceAccountEventError> {
        let endpoint = format!(
            "{}{}",
            cfg.application_url.trim_end_matches('/'),
            SERVICE_ACCOUNT_EVENT_PATH
        );
        let master_key = HeaderValue::from_str(&cfg.application_master_key).map_err(|e| {
            ServiceAccountEventError::Config(format!(
                "application_master_key not header-safe: {e}"
            ))
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| {
                ServiceAccountEventError::Config(format!("reqwest builder: {e}"))
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

    /// Post one event to the application. Returns the parsed ack on success.
    pub async fn send(
        &self,
        event: &ServiceAccountEvent,
    ) -> Result<ServiceAccountEventAck, ServiceAccountEventError> {
        let header_name: HeaderName =
            SERVICE_KEY_HEADER.parse().expect("static header name");
        let response = self
            .http
            .post(&self.endpoint)
            .header(header_name, self.master_key.clone())
            .json(event)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ServiceAccountEventError::Timeout
                } else {
                    ServiceAccountEventError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let raw = response.text().await.map_err(|e| {
            ServiceAccountEventError::Transport(format!("read body: {e}"))
        })?;

        if !status.is_success() {
            let body = parse_ack(&raw);
            return Err(ServiceAccountEventError::AppHttpError {
                status,
                body,
                raw,
            });
        }

        parse_ack(&raw).ok_or_else(|| ServiceAccountEventError::AppMalformed {
            raw: raw.clone(),
            error: format!(
                "body could not be parsed as flat ServiceAccountEventAck nor \
                 as ApiEnvelope-wrapped variant: {raw}"
            ),
        })
    }
}

/// Tolerant parse: accept flat `ServiceAccountEventAck` (spec §5.3 shape) or
/// the ApiEnvelope-wrapped `{code,message,data: ServiceAccountEventAck}` shape
/// that Neton's adapter currently emits.
fn parse_ack(raw: &str) -> Option<ServiceAccountEventAck> {
    if let Ok(a) = serde_json::from_str::<ServiceAccountEventAck>(raw) {
        return Some(a);
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let inner = value.get("data")?;
    serde_json::from_value::<ServiceAccountEventAck>(inner.clone()).ok()
}
