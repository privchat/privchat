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

//! Integration tests: `POST /api/service/users` v1.2 contract.
//!
//! 钉死 USER_API §3 的接口契约：
//! - identity-required (全空 → 400)
//! - 多键幂等 (phone/email/username 任一命中同 uid → 200 + created=false)
//! - identity-conflict (多键命中不同 uid → 409)
//! - existing 不覆盖
//! - phone 强制 E.164
//! - 双前缀 /api/admin + /api/service 等价
//! - 并发同 phone → DB 1 行
//!
//! 这些测试**默认 `#[ignore]`**，要求：
//! 1. PostgreSQL 已应用 migration 014
//! 2. server 在监听（默认 `http://localhost:9090`，可用 `PRIVCHAT_TEST_BASE_URL` 覆盖）
//! 3. service master key（默认 `your_service_master_key_here`，可用
//!    `PRIVCHAT_TEST_SERVICE_KEY` 覆盖）
//!
//! 跑法：
//! ```bash
//! cargo test --test admin_user_create_test -- --ignored --test-threads=1
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

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    // 取后 12 位作为 phone 后缀（保证 E.164 整体长度合法）
    format!("{:012}", nanos % 1_000_000_000_000)
}

fn unique_phone(prefix: &str) -> String {
    // E.164：`+` + 国家码 + 号码。prefix 含国家码，用 unique_suffix 填号码。
    format!("{}{}", prefix, &unique_suffix()[..9])
}

fn unique_email() -> String {
    format!("test_{}@privchat.test", &unique_suffix()[..10])
}

fn unique_username() -> String {
    format!("test_user_{}", &unique_suffix()[..10])
}

async fn post_create_user(
    client: &Client,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let url = format!("{}{}", base_url(), path);
    let resp = client
        .post(&url)
        .header("X-Service-Key", service_key())
        .json(&body)
        .send()
        .await
        .expect("HTTP request failed");
    let status = resp.status();
    let json: Value = resp.json().await.expect("response JSON parse failed");
    (status, json)
}

async fn post_admin(client: &Client, body: Value) -> (StatusCode, Value) {
    post_create_user(client, "/api/admin/users", body).await
}

async fn post_service(client: &Client, body: Value) -> (StatusCode, Value) {
    post_create_user(client, "/api/service/users", body).await
}

// =====================================================
// T1: phone 重复 → 同 uid + created=false
// =====================================================
#[tokio::test]
#[ignore]
async fn t1_phone_idempotent() {
    let client = Client::new();
    let phone = unique_phone("+8613");
    let (s1, b1) = post_service(&client, json!({"phone": phone, "display_name": "alice"})).await;
    let (s2, b2) = post_service(&client, json!({"phone": phone, "display_name": "bob"})).await;

    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1["created"], json!(true));
    assert_eq!(b2["created"], json!(false));
    assert_eq!(b1["user_id"], b2["user_id"], "重复 phone 必须返回相同 uid");
}

// =====================================================
// T2: email 重复 → 同 uid + created=false
// =====================================================
#[tokio::test]
#[ignore]
async fn t2_email_idempotent() {
    let client = Client::new();
    let email = unique_email();
    let (s1, b1) = post_service(&client, json!({"email": email})).await;
    let (s2, b2) = post_service(&client, json!({"email": email})).await;

    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1["created"], json!(true));
    assert_eq!(b2["created"], json!(false));
    assert_eq!(b1["user_id"], b2["user_id"]);
}

// =====================================================
// T3: username 重复 → 同 uid + created=false
// =====================================================
#[tokio::test]
#[ignore]
async fn t3_username_idempotent() {
    let client = Client::new();
    let username = unique_username();
    let (s1, b1) = post_service(&client, json!({"username": username})).await;
    let (s2, b2) = post_service(&client, json!({"username": username})).await;

    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1["created"], json!(true));
    assert_eq!(b2["created"], json!(false));
    assert_eq!(b1["user_id"], b2["user_id"]);
}

// =====================================================
// T4: 三者全空 → 400 INVALID_USER_IDENTITY
// =====================================================
#[tokio::test]
#[ignore]
async fn t4_empty_identity_rejected() {
    let client = Client::new();
    let (s, b) = post_service(&client, json!({})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let msg = b["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("INVALID_USER_IDENTITY"),
        "expected INVALID_USER_IDENTITY, got: {}",
        msg
    );
}

#[tokio::test]
#[ignore]
async fn t4b_only_display_name_rejected() {
    let client = Client::new();
    let (s, b) = post_service(&client, json!({"display_name": "alice"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(b["message"]
        .as_str()
        .unwrap_or("")
        .contains("INVALID_USER_IDENTITY"));
}

// =====================================================
// T5: phone 不带 + → 400 INVALID_PHONE_FORMAT
// =====================================================
#[tokio::test]
#[ignore]
async fn t5_phone_format_rejected() {
    let client = Client::new();
    let (s, b) = post_service(&client, json!({"phone": "13800000000"})).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(b["message"]
        .as_str()
        .unwrap_or("")
        .contains("INVALID_PHONE_FORMAT"));
}

// =====================================================
// T6: phone E.164 合法 → 200
// =====================================================
#[tokio::test]
#[ignore]
async fn t6_phone_e164_accepted() {
    let client = Client::new();
    let phone = unique_phone("+1415");
    let (s, b) = post_service(&client, json!({"phone": phone})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["created"], json!(true));
    assert_eq!(b["phone"], json!(phone));
}

// =====================================================
// T7: phone-only → DB username = NULL
// =====================================================
#[tokio::test]
#[ignore]
async fn t7_phone_only_username_null() {
    let client = Client::new();
    let phone = unique_phone("+8615");
    let (s, b) = post_service(&client, json!({"phone": phone})).await;
    assert_eq!(s, StatusCode::OK);
    assert!(b["username"].is_null(), "phone-only 创建 username 应为 null");
}

// =====================================================
// T8: user_type=2 创建 Bot
// =====================================================
#[tokio::test]
#[ignore]
async fn t8_user_type_bot_persisted() {
    let client = Client::new();
    let username = unique_username();
    let (s, b) = post_service(&client, json!({"username": username, "user_type": 2})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["user_type"], json!(2));
}

// =====================================================
// T9: business_system_id 落库 + 读回
// =====================================================
#[tokio::test]
#[ignore]
async fn t9_business_system_id_persisted() {
    let client = Client::new();
    let email = unique_email();
    let (s, b) = post_service(
        &client,
        json!({"email": email, "business_system_id": "shop_test"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["business_system_id"], json!("shop_test"));
}

// =====================================================
// T10: 并发同 phone × 5 → 1 created + 4 existing，全部同 uid
// =====================================================
#[tokio::test]
#[ignore]
async fn t10_concurrent_same_phone_single_uid() {
    let phone = unique_phone("+8619");
    let mut handles = Vec::new();
    for _ in 0..5 {
        let phone = phone.clone();
        handles.push(tokio::spawn(async move {
            let client = Client::new();
            post_service(&client, json!({"phone": phone})).await
        }));
    }

    let mut uids = Vec::new();
    let mut created_count = 0;
    let mut existing_count = 0;
    for h in handles {
        let (status, body) = h.await.unwrap();
        assert_eq!(status, StatusCode::OK, "并发请求都应该返回 200, body={body}");
        uids.push(body["user_id"].as_u64().unwrap());
        if body["created"] == json!(true) {
            created_count += 1;
        } else {
            existing_count += 1;
        }
    }

    let first = uids[0];
    assert!(uids.iter().all(|u| *u == first), "所有并发请求 uid 必须一致");
    assert_eq!(created_count, 1, "exactly 1 created=true");
    assert_eq!(existing_count, 4, "exactly 4 created=false");
}

// =====================================================
// T11: /api/admin 与 /api/service 双前缀跨前缀幂等
// =====================================================
#[tokio::test]
#[ignore]
async fn t11_dual_prefix_cross_idempotent() {
    let client = Client::new();
    let phone = unique_phone("+8617");
    let (s1, b1) = post_admin(&client, json!({"phone": phone})).await;
    let (s2, b2) = post_service(&client, json!({"phone": phone})).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b1["created"], json!(true));
    assert_eq!(b2["created"], json!(false));
    assert_eq!(b1["user_id"], b2["user_id"]);
}

// =====================================================
// T12: existing 命中 不覆盖 display_name / business_system_id
// =====================================================
#[tokio::test]
#[ignore]
async fn t12_existing_hit_does_not_overwrite() {
    let client = Client::new();
    let phone = unique_phone("+8618");
    let (_, b1) = post_service(
        &client,
        json!({
            "phone": phone,
            "display_name": "first",
            "business_system_id": "first_system"
        }),
    )
    .await;
    let (_, b2) = post_service(
        &client,
        json!({
            "phone": phone,
            "display_name": "OVERRIDE",
            "business_system_id": "OVERRIDE_SYSTEM"
        }),
    )
    .await;

    assert_eq!(b1["user_id"], b2["user_id"]);
    assert_eq!(b2["created"], json!(false));
    assert_eq!(b2["display_name"], json!("first"), "display_name 不应被覆盖");
    assert_eq!(
        b2["business_system_id"],
        json!("first_system"),
        "business_system_id 不应被覆盖"
    );
}

// =====================================================
// T13: phone(A) + email(B) 命中不同 uid → 409 IDENTITY_CONFLICT
// =====================================================
#[tokio::test]
#[ignore]
async fn t13_identity_conflict_phone_vs_email() {
    let client = Client::new();
    let phone_a = unique_phone("+8613");
    let email_b = unique_email();
    let (_, ba) = post_service(&client, json!({"phone": phone_a})).await;
    let (_, bb) = post_service(&client, json!({"email": email_b})).await;
    let uid_a = ba["user_id"].as_u64().unwrap();
    let uid_b = bb["user_id"].as_u64().unwrap();
    assert_ne!(uid_a, uid_b, "setup: A 和 B 必须是不同的 uid");

    let (s, b) = post_service(
        &client,
        json!({"phone": phone_a, "email": email_b}),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let msg = b["message"].as_str().unwrap_or("");
    assert!(
        msg.contains("IDENTITY_CONFLICT"),
        "expected IDENTITY_CONFLICT, got: {}",
        msg
    );
    // message 里应能看到两个冲突的 uid
    assert!(
        msg.contains(&uid_a.to_string()) && msg.contains(&uid_b.to_string()),
        "message 应列出冲突的 uid {} 和 {}, got: {}",
        uid_a,
        uid_b,
        msg
    );
}

// =====================================================
// T14: phone(A) + 新 username → 命中 A 一处，返 Existing(A)，不冲突
// =====================================================
#[tokio::test]
#[ignore]
async fn t14_partial_match_returns_existing() {
    let client = Client::new();
    let phone = unique_phone("+8616");
    let (_, b1) = post_service(&client, json!({"phone": phone})).await;
    let uid_a = b1["user_id"].as_u64().unwrap();

    let new_username = unique_username();
    let (s, b) =
        post_service(&client, json!({"phone": phone, "username": new_username})).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["created"], json!(false));
    assert_eq!(b["user_id"].as_u64().unwrap(), uid_a);
    // existing 不覆盖：username 仍然是 NULL（因为 user A 创建时 username 没传）
    assert!(
        b["username"].is_null(),
        "existing 命中时 username 不应被新值覆盖"
    );
}
