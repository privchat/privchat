// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `/api/service/auth/*` v1.3 unified token endpoints
//! （spec TOKEN_UNIFICATION_SPEC v1.3 §6.1）。
//!
//! - `POST /issue`      — service-only；签 access + refresh，refresh 落库 hash
//! - `POST /refresh`    — service-only；不 rotate，只回新 access + 更新 last_used_at
//! - `POST /introspect` — service-only；权威 active=true/false，不缓存
//! - `POST /revoke`     — service-only；by jti or by refresh_token plaintext
//!
//! 路由 trait：所有 handler 都返回 `ApiResult<T>`，错误 envelope 由 `ServerError`
//! 走 `IntoResponse` 统一映射 protocol::ErrorCode 数字（见 src/http/envelope.rs）。

use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::auth::{IntrospectResult, IssueParams, IssueResult, RevokeRequest};
use crate::error::ServerError;
use crate::http::{AdminServerState, ApiEnvelope, ApiResult};

use super::admin::{extract_service_key, verify_service_key};

/// 注册 unified auth 路由。
pub fn create_route() -> Router<AdminServerState> {
    Router::new()
        .route("/issue", post(issue_handler))
        .route("/refresh", post(refresh_handler))
        .route("/introspect", post(introspect_handler))
        .route("/revoke", post(revoke_handler))
}

// =====================================================
// /issue
// =====================================================

#[derive(Debug, Deserialize)]
pub struct IssueAuthTokenRequest {
    pub user_id: u64,
    /// 客户端持有 device_id 时传入；缺省 server 生成 UUID
    #[serde(default)]
    pub device_id: Option<String>,
    pub device_info: crate::auth::DeviceInfo,
    /// 默认 `["user"]`
    #[serde(default)]
    pub scope: Option<Vec<String>>,
    /// 默认 RsaJwtConfig.default_audience
    #[serde(default)]
    pub audience: Option<Vec<String>>,
    /// 默认 `"privchat-application"`
    #[serde(default)]
    pub business_system_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub user_id: u64,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub refresh_expires_in: i64,
    pub device_id: String,
    pub session_version: i64,
    pub device_created: bool,
    pub scope: Vec<String>,
    pub audience: Vec<String>,
    pub issuer: String,
    pub kid: String,
}

impl From<IssueResult> for LoginResponse {
    fn from(r: IssueResult) -> Self {
        Self {
            user_id: r.user_id,
            access_token: r.access_token,
            refresh_token: r.refresh_token,
            token_type: r.token_type,
            expires_in: r.expires_in,
            refresh_expires_in: r.refresh_expires_in,
            device_id: r.device_id,
            session_version: r.session_version,
            device_created: r.device_created,
            scope: r.scope,
            audience: r.audience,
            issuer: r.issuer,
            kid: r.kid,
        }
    }
}

async fn issue_handler(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<IssueAuthTokenRequest>,
) -> ApiResult<LoginResponse> {
    verify_service_key(&headers, &state).await?;
    let svc = require_unified_token_service(&state)?;

    let params = IssueParams {
        user_id: request.user_id,
        device_id: request.device_id,
        device_info: request.device_info,
        scope: request.scope,
        audience: request.audience,
        business_system_id: request.business_system_id,
        access_ttl_secs: None,
        ip_address: extract_service_key(&headers)
            .ok()
            .map(|_| "0.0.0.0".to_string())
            .unwrap_or_else(|| "0.0.0.0".to_string()),
    };

    let result = svc.issue(params).await?;
    Ok(ApiEnvelope::ok(LoginResponse::from(result)))
}

// =====================================================
// /refresh
// =====================================================

#[derive(Debug, Deserialize)]
pub struct RefreshAuthTokenRequest {
    pub refresh_token: String,
    pub device_id: String,
}

async fn refresh_handler(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<RefreshAuthTokenRequest>,
) -> ApiResult<LoginResponse> {
    verify_service_key(&headers, &state).await?;
    let svc = require_unified_token_service(&state)?;

    if request.refresh_token.trim().is_empty() {
        return Err(ServerError::Validation(
            "refresh_token 不能为空".to_string(),
        ));
    }
    if request.device_id.trim().is_empty() {
        return Err(ServerError::Validation("device_id 不能为空".to_string()));
    }

    let result = svc
        .refresh(&request.refresh_token, &request.device_id)
        .await?;
    Ok(ApiEnvelope::ok(LoginResponse::from(result)))
}

// =====================================================
// /introspect
// =====================================================

#[derive(Debug, Deserialize)]
pub struct IntrospectRequest {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct IntrospectResponse {
    /// 业务结果：`true` = 当前可用；`false` = 已失效（看 reason）
    pub active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
    /// `expired` / `revoked` / `version_mismatch` / `signature_invalid` /
    /// `unknown_kid` / `not_found`
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<IntrospectResult> for IntrospectResponse {
    fn from(r: IntrospectResult) -> Self {
        Self {
            active: r.active,
            user_id: r.user_id,
            device_id: r.device_id,
            session_version: r.session_version,
            scope: r.scope,
            audience: r.audience,
            expires_at: r.expires_at,
            jti: r.jti,
            reason: r.reason,
        }
    }
}

/// `/introspect` **业务上**永远成功；token 失效仅通过 `active=false + reason` 表达，
/// envelope code 仍是 0。只有请求格式 / service key 错才走 4xx envelope。
async fn introspect_handler(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<IntrospectRequest>,
) -> ApiResult<IntrospectResponse> {
    verify_service_key(&headers, &state).await?;
    let svc = require_unified_token_service(&state)?;

    if request.token.trim().is_empty() {
        return Err(ServerError::Validation("token 不能为空".to_string()));
    }

    let result = svc.introspect(&request.token).await;
    Ok(ApiEnvelope::ok(IntrospectResponse::from(result)))
}

// =====================================================
// /revoke
// =====================================================

#[derive(Debug, Deserialize)]
pub struct RevokeAuthRequest {
    /// 至少传 `jti` 或 `refresh_token` 之一
    #[serde(default)]
    pub jti: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RevokeAuthResponse {
    pub revoked_count: u64,
}

async fn revoke_handler(
    State(state): State<AdminServerState>,
    headers: HeaderMap,
    Json(request): Json<RevokeAuthRequest>,
) -> ApiResult<RevokeAuthResponse> {
    verify_service_key(&headers, &state).await?;
    let svc = require_unified_token_service(&state)?;

    let req = match (request.jti, request.refresh_token) {
        (Some(jti), None) if !jti.trim().is_empty() => RevokeRequest::ByJti { jti },
        (None, Some(rt)) if !rt.trim().is_empty() => {
            RevokeRequest::ByRefreshToken { refresh_token: rt }
        }
        (Some(_), Some(_)) => {
            return Err(ServerError::Validation(
                "jti 与 refresh_token 互斥，只能传其一".to_string(),
            ));
        }
        _ => {
            return Err(ServerError::Validation(
                "jti 与 refresh_token 必须传其一".to_string(),
            ));
        }
    };

    let revoked_count = svc.revoke(req).await?;
    Ok(ApiEnvelope::ok(RevokeAuthResponse { revoked_count }))
}

// =====================================================
// 共用：service 未配置时统一返 503
// =====================================================

fn require_unified_token_service(
    state: &AdminServerState,
) -> Result<&std::sync::Arc<crate::auth::UnifiedTokenService>, ServerError> {
    Ok(&state.unified_token_service)
}
