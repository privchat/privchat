// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Outbound HTTP client for `POST /service/privchat/server-event/dispatch`
//! (spec `02-server/SERVER_EVENT_DISPATCH_SPEC` §4).
//!
//! 与 [`crate::channel_transfer::relay_client::ChannelTransferRelayClient`]
//! 形态一致——同样的 timeout / `X-Service-Key` 语义。失败语义：best-effort，
//! 调用方 warn-log 即可，不重试（spec §6）。

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};
use reqwest::StatusCode;

use crate::config::ServerEventConfig;
use crate::server_event::types::{
    ServerEvent, ServerEventAck, SERVER_EVENT_DISPATCH_PATH, SERVICE_KEY_HEADER,
};

/// 投递失败的原因。
#[derive(Debug)]
pub enum ServerEventError {
    /// 下游可达但返回 non-2xx；解析到的 ack（如果有）一并带回。
    AppHttpError {
        status: StatusCode,
        body: Option<ServerEventAck>,
        raw: String,
    },
    /// HTTP 2xx 但 body 无法解析为 `ServerEventAck`。
    AppMalformed { raw: String, error: String },
    /// reqwest 超时。
    Timeout,
    /// 传输层失败（连接拒绝 / DNS / TLS 错等）。
    Transport(String),
    /// 构造时配置错误（bad URL / 不合法 header value）。
    Config(String),
}

impl std::fmt::Display for ServerEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AppHttpError { status, .. } => write!(f, "downstream returned HTTP {status}"),
            Self::AppMalformed { error, .. } => write!(f, "downstream response malformed: {error}"),
            Self::Timeout => write!(f, "downstream call timed out"),
            Self::Transport(msg) => write!(f, "transport error: {msg}"),
            Self::Config(msg) => write!(f, "client misconfigured: {msg}"),
        }
    }
}

impl std::error::Error for ServerEventError {}

/// HTTP caller for `POST /service/privchat/server-event/dispatch`.
///
/// 一个 client 对应一个下游 application（按 `application_url` + 同一把
/// `service_master_key` 配置）。Server 端在不同业务点（bot follow / SendMessage
/// 持久化 / 用户生命周期等）调同一个 client 的 [`send`](Self::send)，传不同
/// `ServerEvent` 即可。
pub struct ServerEventClient {
    http: reqwest::Client,
    endpoint: String,
    master_key: HeaderValue,
}

impl ServerEventClient {
    /// Build a client from configuration.
    pub fn new(cfg: &ServerEventConfig) -> Result<Self, ServerEventError> {
        let endpoint = format!(
            "{}{}",
            cfg.application_url.trim_end_matches('/'),
            SERVER_EVENT_DISPATCH_PATH
        );
        let master_key = HeaderValue::from_str(&cfg.application_master_key).map_err(|e| {
            ServerEventError::Config(format!("application_master_key not header-safe: {e}"))
        })?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_millis(cfg.timeout_ms))
            .build()
            .map_err(|e| ServerEventError::Config(format!("reqwest builder: {e}")))?;
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

    /// Post one event to the downstream. Returns the parsed ack on success.
    pub async fn send(&self, event: &ServerEvent) -> Result<ServerEventAck, ServerEventError> {
        let header_name: HeaderName = SERVICE_KEY_HEADER.parse().expect("static header name");
        let response = self
            .http
            .post(&self.endpoint)
            .header(header_name, self.master_key.clone())
            .json(event)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ServerEventError::Timeout
                } else {
                    ServerEventError::Transport(e.to_string())
                }
            })?;

        let status = response.status();
        let raw = response
            .text()
            .await
            .map_err(|e| ServerEventError::Transport(format!("read body: {e}")))?;

        if !status.is_success() {
            let body = parse_ack(&raw);
            return Err(ServerEventError::AppHttpError { status, body, raw });
        }

        parse_ack(&raw).ok_or_else(|| ServerEventError::AppMalformed {
            raw: raw.clone(),
            error: format!(
                "body could not be parsed as flat ServerEventAck nor as ApiEnvelope-wrapped \
                 variant: {raw}"
            ),
        })
    }
}

/// Tolerant parse: accept flat `ServerEventAck` (spec §4.3 shape) or the
/// ApiEnvelope-wrapped `{code,message,data: ServerEventAck}` shape that Neton's
/// adapter currently emits.
fn parse_ack(raw: &str) -> Option<ServerEventAck> {
    if let Ok(a) = serde_json::from_str::<ServerEventAck>(raw) {
        return Some(a);
    }
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let inner = value.get("data")?;
    serde_json::from_value::<ServerEventAck>(inner.clone()).ok()
}
