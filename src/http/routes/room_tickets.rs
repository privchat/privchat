// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `POST /api/service/room-tickets/issue` — Room subscribe ticket 签发。
//!
//! 业务 API（如 `neton-application-module-game`）在收到玩家 join 请求时
//! 做完业务权限判定（房间存在 + 用户合法 + 座位分配等）后调用本端点拿到
//! 一张 HS256 JWT ticket，再回给客户端用作 `SubscribeRequest.param`。
//!
//! Spec: `02-server/ROOM_CHANNEL_SPEC` §4。
//!
//! **为何需要这个 endpoint**：spec §4.5 明确"票据由业务 API 签发"，但
//! application 不应直接持有 server 的 `room_ticket_secret`——否则 secret
//! 在 application / server 两个进程同时存在 = 攻击面加倍。本端点把签发
//! 权聚集在 server，application 走 `X-Service-Key` 申请。
//!
//! 认证：`X-Service-Key`（与现有 `/api/service/*` 端点一致）。
//!
//! 失败：
//! - 401: `X-Service-Key` 缺失或不匹配
//! - 503: server 未配 `[room_ticket]` 段（无 secret 无法签）
//! - 400: 请求字段非法（`channel_id=0` / `user_id=0` / `device_id` 空 / `ttl_secs` 越界）
//! - 500: 签发内部错误（理论不会出，HS256 + 合法 claims 不会失败）

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::config::RoomTicketConfig;
use crate::http::envelope::ApiEnvelope;
use crate::http::AdminServerState;
use crate::security::room_ticket::{self, RoomTicketClaims, SCOPE_SUBSCRIBE};

/// 最小 / 最大 TTL（秒）。spec §4.3 推荐 5 分钟，给业务一点弹性。
const MIN_TTL_SECS: u64 = 30;
const MAX_TTL_SECS: u64 = 3600;
/// `ttl_secs` 省略时的默认值。
const DEFAULT_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Deserialize)]
pub struct IssueTicketRequest {
    /// 目标 Room channel id。必须 > 0；server 不在这里二次校验是否真实存在
    /// （那是业务 API 的职责），只把它写进 claims `cid`。
    pub channel_id: u64,
    /// ticket 持有者 user_id；写进 `sub`。
    pub user_id: u64,
    /// 持有者 device_id；写进 `did`（spec §4.3：绑设备而非 session）。
    pub device_id: String,
    /// 固定 `"subscribe"`；其它值拒绝（spec §4.3）。
    /// `None` 时默认 `"subscribe"`，便于 application 不写也能用。
    #[serde(default)]
    pub scope: Option<String>,
    /// 过期时间（秒）。`None` 时用默认 300；超过 [MIN_TTL_SECS, MAX_TTL_SECS]
    /// 时返 400。
    #[serde(default)]
    pub ttl_secs: Option<u64>,
    /// （可选）指定签发用的 `kid`，便于多 key 轮换。`None` 时用 config.default_kid。
    #[serde(default)]
    pub kid: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct IssueTicketResponse {
    pub ticket: String,
    /// Echo back，便于客户端 / 业务侧调试 / 日志。
    pub channel_id: u64,
    pub user_id: u64,
    /// 绝对过期时间（Unix 秒）；客户端可用来调度 refresh。
    pub exp: u64,
}

async fn handle_issue(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(req): Json<IssueTicketRequest>,
) -> Response {
    if let Err(resp) = super::admin::verify_service_key(&headers, &state).await {
        return resp.into_response();
    }

    // 401 / 服务-未配 — 503
    let Some(cfg) = state.room_ticket.as_deref() else {
        return error_envelope(
            StatusCode::SERVICE_UNAVAILABLE,
            10100,
            "room_ticket not configured on server",
        );
    };

    if let Err(msg) = validate(&req) {
        return error_envelope(StatusCode::BAD_REQUEST, 10100, msg);
    }

    let ttl = req.ttl_secs.unwrap_or(DEFAULT_TTL_SECS);
    let claims = RoomTicketClaims::for_subscribe(req.user_id, req.device_id, req.channel_id, ttl);
    let kid = req.kid.unwrap_or_else(|| cfg.default_kid.clone());

    match sign_with_kid(cfg, &kid, &claims) {
        Ok(token) => ok_envelope(IssueTicketResponse {
            ticket: token,
            channel_id: claims.cid,
            user_id: req.user_id,
            exp: claims.exp,
        }),
        Err(err) => error_envelope(StatusCode::BAD_REQUEST, 10100, err),
    }
}

fn validate(req: &IssueTicketRequest) -> Result<(), String> {
    if req.channel_id == 0 {
        return Err("channel_id must be > 0".to_string());
    }
    if req.user_id == 0 {
        return Err("user_id must be > 0".to_string());
    }
    let device = req.device_id.trim();
    if device.is_empty() {
        return Err("device_id must not be empty".to_string());
    }
    if device.len() > 128 {
        return Err("device_id too long (max 128)".to_string());
    }
    if let Some(scope) = req.scope.as_deref() {
        if scope != SCOPE_SUBSCRIBE {
            return Err(format!(
                "scope must be '{SCOPE_SUBSCRIBE}', got '{scope}'"
            ));
        }
    }
    if let Some(ttl) = req.ttl_secs {
        if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&ttl) {
            return Err(format!(
                "ttl_secs out of range [{MIN_TTL_SECS}, {MAX_TTL_SECS}]: got {ttl}"
            ));
        }
    }
    Ok(())
}

fn sign_with_kid(
    cfg: &RoomTicketConfig,
    kid: &str,
    claims: &RoomTicketClaims,
) -> Result<String, String> {
    room_ticket::sign(cfg, kid, claims).map_err(|e| {
        if e == "unknown kid" {
            format!("unknown kid '{kid}' — check [room_ticket].keys / default_kid")
        } else {
            format!("ticket sign failed: {e}")
        }
    })
}

fn ok_envelope(data: IssueTicketResponse) -> Response {
    (StatusCode::OK, Json(ApiEnvelope::ok(serde_json::json!(data)))).into_response()
}

fn error_envelope(status: StatusCode, code: u32, message: impl Into<String>) -> Response {
    (
        status,
        Json(ApiEnvelope::<()>::err_raw(code, message.into())),
    )
        .into_response()
}

pub fn create_route() -> Router<AdminServerState> {
    Router::new().route("/issue", post(handle_issue))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(channel_id: u64, user_id: u64, device_id: &str) -> IssueTicketRequest {
        IssueTicketRequest {
            channel_id,
            user_id,
            device_id: device_id.to_string(),
            scope: None,
            ttl_secs: None,
            kid: None,
        }
    }

    #[test]
    fn validate_happy() {
        assert!(validate(&req(1, 1, "d")).is_ok());
    }

    #[test]
    fn validate_rejects_zero_channel() {
        assert!(validate(&req(0, 1, "d")).is_err());
    }

    #[test]
    fn validate_rejects_zero_user() {
        assert!(validate(&req(1, 0, "d")).is_err());
    }

    #[test]
    fn validate_rejects_empty_device() {
        assert!(validate(&req(1, 1, "")).is_err());
        assert!(validate(&req(1, 1, "   ")).is_err());
    }

    #[test]
    fn validate_rejects_long_device() {
        let long = "x".repeat(129);
        assert!(validate(&req(1, 1, &long)).is_err());
    }

    #[test]
    fn validate_rejects_bad_scope() {
        let mut r = req(1, 1, "d");
        r.scope = Some("publish".to_string());
        assert!(validate(&r).is_err());
    }

    #[test]
    fn validate_accepts_explicit_subscribe_scope() {
        let mut r = req(1, 1, "d");
        r.scope = Some(SCOPE_SUBSCRIBE.to_string());
        assert!(validate(&r).is_ok());
    }

    #[test]
    fn validate_rejects_ttl_out_of_range() {
        let mut r = req(1, 1, "d");
        r.ttl_secs = Some(10);
        assert!(validate(&r).is_err());
        r.ttl_secs = Some(MAX_TTL_SECS + 1);
        assert!(validate(&r).is_err());
    }

    #[test]
    fn sign_then_verify_with_same_config() {
        use std::collections::HashMap;
        let cfg = RoomTicketConfig {
            secret: Some("test-secret-32-bytes-or-more!!!!".to_string()),
            keys: HashMap::new(),
            default_kid: "v1".to_string(),
            leeway_secs: 30,
        };
        let claims = RoomTicketClaims::for_subscribe(100, "dev-a", 9001, 300);
        let token = sign_with_kid(&cfg, "v1", &claims).expect("sign");
        let verified =
            room_ticket::verify(&cfg, &token, 9001, "dev-a").expect("verify");
        assert_eq!(verified.sub, "100");
        assert_eq!(verified.cid, 9001);
        assert_eq!(verified.did, "dev-a");
        assert_eq!(verified.scope, SCOPE_SUBSCRIBE);
        assert_eq!(verified.ct, 2);
    }

    #[test]
    fn sign_unknown_kid_reports_kid() {
        use std::collections::HashMap;
        let mut keys = HashMap::new();
        keys.insert("v1".to_string(), "k1".to_string());
        let cfg = RoomTicketConfig {
            secret: None,
            keys,
            default_kid: "v1".to_string(),
            leeway_secs: 30,
        };
        let claims = RoomTicketClaims::for_subscribe(1, "d", 1, 300);
        let err = sign_with_kid(&cfg, "v99", &claims).unwrap_err();
        assert!(err.contains("v99"));
    }
}
