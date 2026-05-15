// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! `group/qrcode/get` handler — QR_CODE_SPEC v1.3。
//!
//! 读出群当前的 `qr_key`（直接来自 `privchat_groups.qr_key` 列），
//! 不再"生成"——qr_key 在群创建时已自动产生，后续靠 `refresh` 旋转。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::qr::{build_qr_url, QrAction, QrEntity};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::qrcode::{
    GroupQRCodeGetRequest, GroupQRCodeGetResponse,
};
use serde_json::{json, Value};

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 读取群二维码 请求: {:?}", body);

    let request: GroupQRCodeGetRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    let operator_id = crate::rpc::get_current_user_id(&ctx)?;
    let group_id = request.group_id;

    // 1. 拉群信息（顺便鉴权：必须是群成员）
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    if !channel.members.contains_key(&operator_id) {
        return Err(RpcError::forbidden("您不是群组成员".to_string()));
    }

    // 2. 读 qr_key
    let qr_key: String = sqlx::query_scalar::<_, String>(
        "SELECT qr_key FROM privchat_groups WHERE group_id = $1",
    )
    .bind(group_id as i64)
    .fetch_optional(services.channel_service.pool())
    .await
    .map_err(|e| RpcError::internal(format!("读取群 qr_key 失败: {}", e)))?
    .ok_or_else(|| RpcError::not_found("群组不存在".to_string()))?;

    // 3. 用配置的 qr_base_url 拼 URL（normalize 已在启动期跑过）
    let qr_code = build_qr_url(
        &services.config.qr_base_url,
        QrEntity::Group,
        QrAction::Join,
        &qr_key,
    );

    let resp = GroupQRCodeGetResponse {
        qr_key,
        qr_code,
        group_id,
    };
    Ok(json!(resp))
}
