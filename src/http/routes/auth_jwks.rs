// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `GET /api/service/auth/jwks` —— RS256 unified token 公钥发布端点
//! （spec TOKEN_UNIFICATION_SPEC v1.3 §6.1）。
//!
//! **公开**端点，**不**走 `verify_service_key`：JWKS 本来就是公钥，
//! 安全边界在私钥不在公钥（spec §11.1）。
//!
//! 后续要补 `/.well-known/jwks.json` 的话，挂在同一个 router 上即可。

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{extract::State, Json};

use crate::http::AdminServerState;

/// 返回 server 当前签名公钥集（Phase A 单 key）。
///
/// 未配置 `[auth.rsa_jwt]`（生产部署关闭 unified token 时）返回 503，
/// 避免误导 application 以为 server 支持但没数据。
pub async fn handler(State(state): State<AdminServerState>) -> impl IntoResponse {
    match state.rsa_jwt_service.as_ref() {
        Some(svc) => (StatusCode::OK, Json(svc.jwks())).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "rsa_jwt_not_configured",
                "message": "Unified token (RS256) is not configured on this server. \
                            Set [auth.rsa_jwt] in config or JWT_RS256_* env vars."
            })),
        )
            .into_response(),
    }
}
