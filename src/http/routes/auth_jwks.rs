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

use crate::config::JwtAlgorithm;
use crate::http::AdminServerState;

/// 返回 server 当前签名公钥集。
///
/// 仅 RS256 模式下有 keys；HS256 模式下返回 503（HS256 没有公钥可发布）。
pub async fn handler(State(state): State<AdminServerState>) -> impl IntoResponse {
    let token_service = state.unified_token_service.token_service();
    match token_service.algorithm() {
        JwtAlgorithm::Rs256 => (StatusCode::OK, Json(token_service.jwks())).into_response(),
        JwtAlgorithm::Hs256 => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "jwks_unavailable_for_hs256",
                "message": "[auth.jwt] algorithm=HS256；HS256 不暴露公钥。改 RS256 后此端点会返回 JWKS。"
            })),
        )
            .into_response(),
    }
}
