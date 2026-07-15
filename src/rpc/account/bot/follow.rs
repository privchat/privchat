// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `account/bot/follow` handler (spec §3.1).

use crate::model::user::UserStatus;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::server_event::types::ServerEvent;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::rpc::account::bot::{BotFollowRequest, BotFollowResponse};
use serde_json::{json, Value};

/// Bot user_type sentinel — kept in sync with `model::user` comment "2=机器人".
const USER_TYPE_BOT: i16 = 2;

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let request: BotFollowRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let bot_user_id = request.bot_user_id;

    if bot_user_id == 0 {
        return Err(RpcError::validation("bot_user_id 不能为 0".to_string()));
    }
    if user_id == bot_user_id {
        return Err(RpcError::validation("不能关注自己".to_string()));
    }

    // Validate target — full User row needed (we check status, not just profile).
    let target = services
        .user_repository
        .find_by_id(bot_user_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询用户失败: {}", e)))?;

    let target = match target {
        Some(u) => u,
        None => {
            return Err(RpcError::from_code(
                ErrorCode::BotNotFound,
                format!("bot {} not found", bot_user_id),
            ))
        }
    };

    if target.user_type != USER_TYPE_BOT {
        return Err(RpcError::from_code(
            ErrorCode::NotABot,
            format!("user {} is not a bot", bot_user_id),
        ));
    }
    if !matches!(target.status, UserStatus::Active) {
        return Err(RpcError::from_code(
            ErrorCode::BotDisabled,
            format!("bot {} is disabled", bot_user_id),
        ));
    }

    // direct channel get-or-create — same as channel/direct/get_or_create.
    let (channel_id, _channel_created) = services
        .channel_service
        .get_or_create_direct_channel(user_id, bot_user_id, Some("bot_follow"), None)
        .await
        .map_err(|e| RpcError::internal(format!("获取或创建会话失败: {}", e)))?;

    let now_ms = chrono::Utc::now().timestamp_millis();

    let outcome = services
        .bot_follow_repository
        .upsert_followed(user_id, bot_user_id, channel_id, now_ms)
        .await
        .map_err(|e| RpcError::internal(format!("写入 bot follow 关系失败: {}", e)))?;

    let created = outcome.is_created();

    // Emit server event `bot.followed` — best-effort, fire-and-forget
    // (spec SERVER_EVENT_DISPATCH_SPEC §6)。Downstream 按 event_type 路由到
    // 自己的 handler 处理 binding / 欢迎语 / 业务副作用。
    if let Some(client) = services.server_event_client.clone() {
        match ServerEvent::bot_followed(
            user_id,
            bot_user_id,
            channel_id,
            i32::from(target.user_type),
            created,
            now_ms,
        ) {
            Ok(event) => {
                tokio::spawn(async move {
                    if let Err(e) = client.send(&event).await {
                        tracing::warn!(
                            target: "bot_follow",
                            "server-event 通知失败 (bot.followed): {}",
                            e
                        );
                    }
                });
            }
            Err(e) => {
                tracing::warn!(
                    target: "bot_follow",
                    "server-event payload 序列化失败 (bot.followed): {}",
                    e
                );
            }
        }
    }

    let response = BotFollowResponse {
        bot_user_id,
        channel_id,
        account_user_type: target.user_type,
        followed: true,
        created,
    };

    tracing::info!(user_id, bot_user_id, channel_id, created, "bot follow ok");

    Ok(json!(response))
}
