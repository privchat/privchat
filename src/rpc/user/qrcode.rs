// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! 个人名片二维码 RPC handler（QR_CODE_SPEC v1.3）。
//!
//! 三个 handler：
//! - `user/qrcode/get` — 读自己的 qr_key + URL
//! - `user/qrcode/refresh` — 旋转自己的 qr_key
//! - `user/qrcode/resolve` — 用对端的 qrkey 拉最小用户卡片
//!
//! 历史 `user/qrcode/generate` 已停用：qr_key 在注册时自动产生，
//! 不需要"生成"动作；"刷新"由 `refresh` 提供。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::qr::{
    build_qr_url, generate_qr_key, is_qr_key_unique_violation, QrAction, QrEntity,
    QR_KEY_MAX_GENERATION_RETRIES,
};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::user_qrcode::{
    UserQRCodeGetResponse, UserQRCodeRefreshResponse, UserQRCodeResolveRequest,
    UserQRCodeResolveResponse,
};
use serde_json::{json, Value};

/// `user/qrcode/get` — 读自己的 qr_key。
pub async fn get(
    _body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let qr_key: String =
        sqlx::query_scalar::<_, String>("SELECT qr_key FROM privchat_users WHERE user_id = $1")
            .bind(user_id as i64)
            .fetch_optional(services.channel_service.pool())
            .await
            .map_err(|e| RpcError::internal(format!("读取 qr_key 失败: {}", e)))?
            .ok_or_else(|| RpcError::not_found("用户不存在".to_string()))?;

    let qr_code = build_qr_url(
        &services.config.qr_base_url,
        QrEntity::User,
        QrAction::Get,
        &qr_key,
    );

    let resp = UserQRCodeGetResponse {
        qr_key,
        qr_code,
        user_id,
    };
    Ok(json!(resp))
}

/// `user/qrcode/refresh` — 旋转自己的 qr_key（用户主动应对骚扰）。
pub async fn refresh(
    _body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let old_qr_key: String =
        sqlx::query_scalar::<_, String>("SELECT qr_key FROM privchat_users WHERE user_id = $1")
            .bind(user_id as i64)
            .fetch_optional(services.channel_service.pool())
            .await
            .map_err(|e| RpcError::internal(format!("读取旧 qr_key 失败: {}", e)))?
            .ok_or_else(|| RpcError::not_found("用户不存在".to_string()))?;

    let mut last_err: Option<sqlx::Error> = None;
    let mut new_qr_key: Option<String> = None;
    for _ in 0..QR_KEY_MAX_GENERATION_RETRIES {
        let candidate = generate_qr_key();
        let attempt = sqlx::query(
            "UPDATE privchat_users SET qr_key = $1, updated_at = $2 WHERE user_id = $3",
        )
        .bind(&candidate)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(user_id as i64)
        .execute(services.channel_service.pool())
        .await;

        match attempt {
            Ok(_) => {
                new_qr_key = Some(candidate);
                break;
            }
            Err(e) if is_qr_key_unique_violation(&e) => {
                last_err = Some(e);
                continue;
            }
            Err(e) => {
                return Err(RpcError::internal(format!("UPDATE qr_key failed: {}", e)));
            }
        }
    }

    let new_qr_key = new_qr_key.ok_or_else(|| {
        RpcError::internal(format!(
            "qr_key UNIQUE 冲突重试 {} 次仍失败: {:?}",
            QR_KEY_MAX_GENERATION_RETRIES, last_err
        ))
    })?;

    let qr_code = build_qr_url(
        &services.config.qr_base_url,
        QrEntity::User,
        QrAction::Get,
        &new_qr_key,
    );

    let resp = UserQRCodeRefreshResponse {
        old_qr_key,
        new_qr_key,
        qr_code,
        user_id,
    };
    Ok(json!(resp))
}

/// `user/qrcode/resolve` — 把对端 qrkey 翻译成最小用户卡片。
///
/// **不返回 qr_key**（避免二次扩散）；返回最少够 UI 展示 + add-friend 用的字段。
pub async fn resolve(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let req: UserQRCodeResolveRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    let operator_id = crate::rpc::get_current_user_id(&ctx)?;

    if req.qr_key.is_empty() {
        return Err(RpcError::validation("qr_key is required".to_string()));
    }

    let row = sqlx::query_as::<_, (i64, String, Option<String>, Option<String>, i16)>(
        r#"
        SELECT user_id, username, display_name, avatar_url, user_type
          FROM privchat_users
         WHERE qr_key = $1
        "#,
    )
    .bind(&req.qr_key)
    .fetch_optional(services.channel_service.pool())
    .await
    .map_err(|e| RpcError::internal(format!("解析名片码失败: {}", e)))?
    .ok_or_else(|| RpcError::not_found("名片码无效或已被旋转".to_string()))?;

    let target_user_id = row.0 as u64;
    let is_self = target_user_id == operator_id;

    // 调用方与目标是否已经是好友 —— is_friend 本身是 best-effort，无 Result。
    let is_friend = if is_self {
        true
    } else {
        services
            .friend_service
            .is_friend(operator_id, target_user_id)
            .await
    };

    let resp = UserQRCodeResolveResponse {
        user_id: target_user_id,
        username: row.1,
        display_name: row.2,
        avatar_url: row.3,
        user_type: row.4,
        is_friend,
        is_self,
    };
    Ok(json!(resp))
}
