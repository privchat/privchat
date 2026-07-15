// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! `group/qrcode/refresh` handler — QR_CODE_SPEC v1.3。
//!
//! Owner / Admin 主动旋转群二维码：`UPDATE privchat_groups
//! SET qr_key=$new WHERE group_id=?`，老 qr_key 立即作废。
//! UNIQUE 约束碰撞 → 重试，最多 5 次。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::qr::{
    build_qr_url, generate_qr_key, is_qr_key_unique_violation, QrAction, QrEntity,
    QR_KEY_MAX_GENERATION_RETRIES,
};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::qrcode::{
    GroupQRCodeRefreshRequest, GroupQRCodeRefreshResponse,
};
use serde_json::{json, Value};

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 刷新群二维码 请求: {:?}", body);

    let request: GroupQRCodeRefreshRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    let operator_id = crate::rpc::get_current_user_id(&ctx)?;
    let group_id = request.group_id;

    // 1. 鉴权：Owner / Admin only
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin,
    ) {
        return Err(RpcError::forbidden(
            "只有群主或管理员可以旋转群二维码".to_string(),
        ));
    }

    // 2. 取旧 qr_key（仅用于响应回显，让客户端知道哪一张失效了）
    let old_qr_key: String =
        sqlx::query_scalar::<_, String>("SELECT qr_key FROM privchat_groups WHERE group_id = $1")
            .bind(group_id as i64)
            .fetch_optional(services.channel_service.pool())
            .await
            .map_err(|e| RpcError::internal(format!("读取旧 qr_key 失败: {}", e)))?
            .ok_or_else(|| RpcError::not_found("群组不存在".to_string()))?;

    // 3. UPDATE 新 qr_key，带 UNIQUE 冲突重试
    let mut last_err: Option<sqlx::Error> = None;
    let mut new_qr_key: Option<String> = None;
    for _ in 0..QR_KEY_MAX_GENERATION_RETRIES {
        let candidate = generate_qr_key();
        let attempt = sqlx::query(
            "UPDATE privchat_groups SET qr_key = $1, updated_at = $2 WHERE group_id = $3",
        )
        .bind(&candidate)
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(group_id as i64)
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

    tracing::debug!(
        "✅ 群二维码已旋转: group_id={}, old={}, new={}",
        group_id,
        old_qr_key,
        new_qr_key
    );

    let qr_code = build_qr_url(
        &services.config.qr_base_url,
        QrEntity::Group,
        QrAction::Join,
        &new_qr_key,
    );

    let resp = GroupQRCodeRefreshResponse {
        old_qr_key,
        new_qr_key,
        qr_code,
        group_id,
    };
    Ok(json!(resp))
}
