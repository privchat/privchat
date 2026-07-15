// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! `group/join/qrcode` handler — QR_CODE_SPEC v1.3。
//!
//! 与历史 `QRCodeService` 解耦：直接 `SELECT group_id FROM privchat_groups
//! WHERE qr_key = $1`，找到群后走与邀请相同的 join_need_approval 流程。
//! 不再用 `&token=` URL 参数，UNIQUE qr_key 本身就是不可枚举的安全凭证。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use privchat_protocol::rpc::group::qrcode::GroupQRCodeJoinRequest;
use serde_json::{json, Value};

/// 处理 扫码加群 请求
///
/// RPC: `group/join/qrcode`
///
/// 请求：
/// ```json
/// { "qr_key": "K7sP3qXfA9eLm2nB", "message": "我想加群" }
/// ```
///
/// 响应（无需审批）：
/// ```json
/// {
///   "status": "joined",
///   "group_id": 12345,
///   "user_id": 100002091,
///   "joined_at": 1715833200000,
///   "message": "已加入群组"
/// }
/// ```
///
/// 响应（需审批）：
/// ```json
/// {
///   "status": "pending",
///   "group_id": 12345,
///   "request_id": "req_456",
///   "message": "申请已提交，等待管理员审批"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 扫码加群 请求: {:?}", body);

    let request: GroupQRCodeJoinRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let qr_key = request.qr_key;
    let message = request.message;

    if qr_key.is_empty() {
        return Err(RpcError::validation("qr_key is required".to_string()));
    }

    // 1. 反查 group_id —— qr_key 是 privchat_groups 表上的 UNIQUE 字段。
    let group_id: u64 =
        sqlx::query_scalar::<_, i64>("SELECT group_id FROM privchat_groups WHERE qr_key = $1")
            .bind(&qr_key)
            .fetch_optional(services.channel_service.pool())
            .await
            .map_err(|e| RpcError::internal(format!("查询群二维码失败: {}", e)))?
            .ok_or_else(|| RpcError::not_found("二维码无效或已被旋转".to_string()))? as u64;

    tracing::debug!(
        "✅ 二维码反查成功: qr_key={}, group_id={}",
        qr_key,
        group_id
    );

    // 2. 验证用户存在（与历史 invite/qrcode 流程一致）
    let _user_profile = helpers::get_user_profile_with_fallback(
        user_id,
        &services.user_repository,
        &services.cache_manager,
    )
    .await
    .map_err(|e| RpcError::internal(format!("获取用户信息失败: {}", e)))?
    .ok_or_else(|| RpcError::not_found(format!("用户不存在: {}", user_id)))?;

    // 3. 拉群信息
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 4. 已是群成员
    if channel.members.contains_key(&user_id) {
        return Err(RpcError::validation("您已经是群成员".to_string()));
    }

    // 5. 群人数已满
    let current_member_count = channel.members.len() as u32;
    let max_members = channel
        .settings
        .as_ref()
        .and_then(|s| s.max_members.map(|m| m as usize))
        .or_else(|| channel.metadata.max_members)
        .unwrap_or(500);
    if current_member_count >= max_members as u32 {
        return Err(RpcError::validation("群组人数已满".to_string()));
    }

    // 6. 审批分流（与邀请共用流程；method = QRCode）
    let need_approval = channel
        .settings
        .as_ref()
        .map(|s| s.require_approval)
        .unwrap_or(false);

    if need_approval {
        tracing::debug!(
            "⏰ 扫码加群需要审批: user_id={}, group_id={}",
            user_id,
            group_id
        );

        let join_request = services
            .approval_service
            .create_join_request(
                group_id,
                user_id,
                crate::service::JoinMethod::QRCode {
                    qr_code_id: qr_key.clone(),
                },
                message,
                Some(72), // 72 小时过期
            )
            .await
            .map_err(|e| RpcError::internal(format!("创建审批请求失败: {}", e)))?;

        Ok(json!({
            "status": "pending",
            "group_id": group_id,
            "request_id": join_request.request_id,
            "message": "申请已提交，等待管理员审批",
            "expires_at": join_request.expires_at.map(|dt| dt.timestamp_millis()),
        }))
    } else {
        services
            .channel_service
            .join_channel(
                group_id,
                user_id,
                Some(crate::model::channel::MemberRole::Member),
            )
            .await
            .map_err(|e| RpcError::internal(format!("加入群组失败: {}", e)))?;

        tracing::debug!(
            "✅ 扫码加群成功: user_id={}, group_id={}",
            user_id,
            group_id
        );

        Ok(json!({
            "status": "joined",
            "group_id": group_id,
            "message": "已加入群组",
            "user_id": user_id,
            "joined_at": chrono::Utc::now().timestamp_millis(),
        }))
    }
}
