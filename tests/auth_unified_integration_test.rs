// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Integration tests: `/api/service/auth/*` v1.3 unified token API
//! （spec TOKEN_UNIFICATION_SPEC v1.3 §6.1, Phase A acceptance §12 G1–G6 第一轮）。
//!
//! 钉死的契约：
//! 1. `/issue` 必须 service-key only（无 key → envelope code=10000 / 401）
//! 2. JWKS 公开（无 service-key 即可）
//! 3. introspect 对 fresh access → `active=true`
//! 4. refresh 成功后 introspect 新 access 仍 `active=true`
//! 5. revoke by jti 后 refresh 路径失败、introspect 该 refresh 应 inactive
//! 6. session_version bump 后旧 access introspect → `version_mismatch`
//! 7. 篡改签名 / unknown kid / expired → 对应 reason
//! 8. revoke 互斥参数：jti 与 refresh_token 同传 / 都不传 → 400
//!
//! 这些测试**默认 `#[ignore]`**，需要：
//! 1. PostgreSQL 已应用 migration 014 + 015
//! 2. server 配置好 `[auth.rsa_jwt]`（RSA 私钥 / 公钥 / kid），并启动监听
//! 3. service master key（默认 `your_service_master_key_here`，可用
//!    `PRIVCHAT_TEST_SERVICE_KEY` 覆盖）
//!
//! 跑法：
//! ```bash
//! cargo test --test auth_unified_integration_test -- --ignored --test-threads=1
//! ```

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};

fn base_url() -> String {
    std::env::var("PRIVCHAT_TEST_BASE_URL").unwrap_or_else(|_| "http://localhost:9090".to_string())
}

fn service_key() -> String {
    std::env::var("PRIVCHAT_TEST_SERVICE_KEY")
        .unwrap_or_else(|_| "your_service_master_key_here".to_string())
}

fn unique_uid() -> u64 {
    // 用纳秒时间 + 偏移；测试相互不冲突即可
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // 限制到 i64 安全区
    100_000_000_000u64 + (nanos as u64 % 1_000_000_000_000u64)
}

struct EnvelopeResp {
    status: StatusCode,
    code: u32,
    message: String,
    data: Value,
}

impl EnvelopeResp {
    fn ok_data(&self) -> &Value {
        assert_eq!(
            self.code, 0,
            "expected envelope.code=0, got {} (msg: {})",
            self.code, self.message
        );
        &self.data
    }
}

async fn post_json(
    client: &Client,
    path: &str,
    body: Value,
    with_service_key: bool,
) -> EnvelopeResp {
    let url = format!("{}{}", base_url(), path);
    let mut req = client.post(&url).json(&body);
    if with_service_key {
        req = req.header("X-Service-Key", service_key());
    }
    let resp = req.send().await.expect("HTTP request failed");
    let status = resp.status();
    let json: Value = resp.json().await.expect("response JSON parse failed");
    let code = json["code"].as_u64().expect("envelope.code missing") as u32;
    let message = json["message"].as_str().unwrap_or("").to_string();
    let data = json["data"].clone();
    EnvelopeResp {
        status,
        code,
        message,
        data,
    }
}

async fn get_no_envelope(client: &Client, path: &str) -> (StatusCode, Value) {
    let url = format!("{}{}", base_url(), path);
    let resp = client.get(&url).send().await.expect("HTTP request failed");
    let status = resp.status();
    let json: Value = resp.json().await.expect("JSON parse");
    (status, json)
}

fn issue_body(uid: u64, device_id: Option<&str>) -> Value {
    let mut body = json!({
        "user_id": uid,
        "device_info": {
            "app_id": "ios",
            "device_name": "Integration Test Device",
            "device_model": "iPhone15,2",
            "os_version": "iOS 18.0",
            "app_version": "1.0.0",
        },
        "scope": ["user"],
    });
    if let Some(d) = device_id {
        body["device_id"] = json!(d);
    }
    body
}

// =====================================================
// Phase A acceptance §12 G1–G6 第一轮
// =====================================================

/// G1：`/issue` 必须 service-key only。
#[tokio::test]
#[ignore]
async fn issue_without_service_key_returns_unauthorized() {
    let client = Client::new();
    let resp = post_json(
        &client,
        "/api/service/auth/issue",
        issue_body(unique_uid(), None),
        false, // 故意不带 service key
    )
    .await;
    assert_eq!(resp.status, StatusCode::UNAUTHORIZED);
    assert_ne!(resp.code, 0, "missing service key 应返非 0 envelope code");
}

/// G2：JWKS 公开访问，无 service-key。返回 RS256 / kty=RSA / 至少一个 key。
#[tokio::test]
#[ignore]
async fn jwks_is_public_and_well_formed() {
    let client = Client::new();
    let (status, body) = get_no_envelope(&client, "/api/service/auth/jwks").await;
    assert_eq!(status, StatusCode::OK);
    let keys = body["keys"]
        .as_array()
        .expect("keys must be array");
    assert!(!keys.is_empty(), "JWKS keys array 不能为空");
    let k = &keys[0];
    assert_eq!(k["kty"].as_str(), Some("RSA"));
    assert_eq!(k["alg"].as_str(), Some("RS256"));
    assert_eq!(k["use"].as_str(), Some("sig"));
    assert!(k["kid"].is_string(), "kid 必须存在");
    assert!(k["n"].is_string(), "n 必须存在");
    assert!(k["e"].is_string(), "e 必须存在");
}

/// G3 + G4 + 第一轮 happy path：
/// issue → introspect access active=true → refresh → introspect 新 access active=true。
#[tokio::test]
#[ignore]
async fn issue_introspect_refresh_round_trip() {
    let client = Client::new();
    let uid = unique_uid();

    // issue
    let r = post_json(&client, "/api/service/auth/issue", issue_body(uid, None), true).await;
    let data = r.ok_data();
    let access = data["access_token"].as_str().expect("access_token").to_string();
    let refresh = data["refresh_token"].as_str().expect("refresh_token").to_string();
    let device_id = data["device_id"].as_str().expect("device_id").to_string();
    assert_eq!(data["user_id"].as_u64(), Some(uid));
    assert_eq!(data["session_version"].as_i64(), Some(1));
    assert_eq!(data["scope"][0].as_str(), Some("user"));
    assert_eq!(data["issuer"].as_str(), Some("privchat-server"));

    // introspect access
    let r = post_json(
        &client,
        "/api/service/auth/introspect",
        json!({ "token": access }),
        true,
    )
    .await;
    let data = r.ok_data();
    assert_eq!(data["active"].as_bool(), Some(true));
    assert_eq!(data["user_id"].as_u64(), Some(uid));
    assert_eq!(data["device_id"].as_str(), Some(device_id.as_str()));
    assert_eq!(data["session_version"].as_i64(), Some(1));

    // refresh
    let r = post_json(
        &client,
        "/api/service/auth/refresh",
        json!({ "refresh_token": refresh, "device_id": device_id }),
        true,
    )
    .await;
    let data = r.ok_data();
    let new_access = data["access_token"].as_str().expect("new access").to_string();
    assert_ne!(new_access, access, "refresh 必须返回新的 access_token");
    // 非 rotation：refresh_token 原样返回
    assert_eq!(data["refresh_token"].as_str(), Some(refresh.as_str()));

    // introspect 新 access 仍 active
    let r = post_json(
        &client,
        "/api/service/auth/introspect",
        json!({ "token": new_access }),
        true,
    )
    .await;
    assert_eq!(r.ok_data()["active"].as_bool(), Some(true));
}

/// G5：revoke by jti 后 refresh 失败、introspect 该 refresh inactive。
#[tokio::test]
#[ignore]
async fn revoke_by_jti_invalidates_refresh() {
    let client = Client::new();
    let uid = unique_uid();

    let r = post_json(&client, "/api/service/auth/issue", issue_body(uid, None), true).await;
    let data = r.ok_data();
    let refresh = data["refresh_token"].as_str().unwrap().to_string();
    let device_id = data["device_id"].as_str().unwrap().to_string();

    // refresh JWT 的 payload.jti（base64url decode）
    let jti = decode_jwt_jti(&refresh).expect("decode refresh jti");

    // revoke
    let r = post_json(
        &client,
        "/api/service/auth/revoke",
        json!({ "jti": jti }),
        true,
    )
    .await;
    let data = r.ok_data();
    assert_eq!(data["revoked_count"].as_u64(), Some(1));

    // refresh now fails
    let r = post_json(
        &client,
        "/api/service/auth/refresh",
        json!({ "refresh_token": refresh, "device_id": device_id }),
        true,
    )
    .await;
    assert_ne!(r.code, 0, "已 revoke 的 refresh 必须 refresh 失败");

    // introspect refresh inactive
    let r = post_json(
        &client,
        "/api/service/auth/introspect",
        json!({ "token": refresh }),
        true,
    )
    .await;
    let data = r.ok_data();
    assert_eq!(data["active"].as_bool(), Some(false));
    assert_eq!(data["reason"].as_str(), Some("revoked"));
}

/// G6：bumpSessions 后旧 access introspect 返 version_mismatch。
#[tokio::test]
#[ignore]
async fn bump_sessions_invalidates_old_access() {
    let client = Client::new();
    let uid = unique_uid();

    let r = post_json(&client, "/api/service/auth/issue", issue_body(uid, None), true).await;
    let data = r.ok_data();
    let access = data["access_token"].as_str().unwrap().to_string();

    // bump
    let bump_url = format!("/api/service/users/{}/sessions/bump", uid);
    let r = client
        .post(format!("{}{}", base_url(), bump_url))
        .header("X-Service-Key", service_key())
        .json(&json!({"reason": "integration_test"}))
        .send()
        .await
        .expect("bump request");
    assert!(
        r.status().is_success(),
        "bump_sessions HTTP failed: {}",
        r.status()
    );

    // introspect 旧 access：active=false reason=version_mismatch
    let r = post_json(
        &client,
        "/api/service/auth/introspect",
        json!({ "token": access }),
        true,
    )
    .await;
    let data = r.ok_data();
    assert_eq!(data["active"].as_bool(), Some(false));
    assert_eq!(data["reason"].as_str(), Some("version_mismatch"));
}

/// 篡改签名 → introspect inactive reason=signature_invalid。
#[tokio::test]
#[ignore]
async fn introspect_tampered_token_returns_signature_invalid() {
    let client = Client::new();
    let uid = unique_uid();
    let r = post_json(&client, "/api/service/auth/issue", issue_body(uid, None), true).await;
    let access = r.ok_data()["access_token"].as_str().unwrap().to_string();

    // 在签名段最后改一字符
    let mut tampered = access.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });

    let r = post_json(
        &client,
        "/api/service/auth/introspect",
        json!({ "token": tampered }),
        true,
    )
    .await;
    let data = r.ok_data();
    assert_eq!(data["active"].as_bool(), Some(false));
    let reason = data["reason"].as_str().unwrap_or("");
    assert!(
        reason == "signature_invalid",
        "expected signature_invalid, got {}",
        reason
    );
}

/// revoke 互斥参数：jti / refresh_token 同传或都不传都返 400。
#[tokio::test]
#[ignore]
async fn revoke_requires_exactly_one_of_jti_or_refresh_token() {
    let client = Client::new();

    // both
    let r = post_json(
        &client,
        "/api/service/auth/revoke",
        json!({ "jti": "x", "refresh_token": "y" }),
        true,
    )
    .await;
    assert_ne!(r.code, 0, "同时传 jti 与 refresh_token 应该 envelope error");

    // neither
    let r = post_json(&client, "/api/service/auth/revoke", json!({}), true).await;
    assert_ne!(r.code, 0, "都不传应该 envelope error");
}

/// `device_id` 与 refresh token claim 不一致：refresh 失败。
#[tokio::test]
#[ignore]
async fn refresh_with_wrong_device_id_fails() {
    let client = Client::new();
    let uid = unique_uid();
    let r = post_json(&client, "/api/service/auth/issue", issue_body(uid, None), true).await;
    let refresh = r.ok_data()["refresh_token"].as_str().unwrap().to_string();

    let r = post_json(
        &client,
        "/api/service/auth/refresh",
        json!({ "refresh_token": refresh, "device_id": "00000000-0000-0000-0000-000000000000" }),
        true,
    )
    .await;
    assert_ne!(r.code, 0, "device_id 不一致必须 refresh 失败");
}

// =====================================================
// helpers
// =====================================================

fn decode_jwt_jti(jwt: &str) -> Option<String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload_bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: Value = serde_json::from_slice(&payload_bytes).ok()?;
    payload["jti"].as_str().map(|s| s.to_string())
}
