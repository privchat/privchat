// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `account/bot/unfollow` handler (spec §3.2).

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::server_event::types::ServerEvent;
use privchat_protocol::rpc::account::bot::{BotUnfollowRequest, BotUnfollowResponse};
use serde_json::{json, Value};

/// Bot user_type sentinel — `account_user_type` carried in event payload.
const USER_TYPE_BOT: i32 = 2;

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let request: BotUnfollowRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let bot_user_id = request.bot_user_id;

    if bot_user_id == 0 {
        return Err(RpcError::validation("bot_user_id 不能为 0".to_string()));
    }

    let existing = services
        .bot_follow_repository
        .find(user_id, bot_user_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询 bot follow 关系失败: {}", e)))?;

    let existing = match existing {
        Some(rec) if rec.is_followed() => rec,
        _ => {
            // 没有 follow 关系或已 unfollow — 幂等返回。
            let response = BotUnfollowResponse {
                bot_user_id,
                channel_id: 0,
                unfollowed: false,
            };
            return Ok(json!(response));
        }
    };

    let now_ms = chrono::Utc::now().timestamp_millis();
    let affected = services
        .bot_follow_repository
        .set_unfollowed(user_id, bot_user_id, now_ms)
        .await
        .map_err(|e| RpcError::internal(format!("更新 bot follow 关系失败: {}", e)))?;

    if affected == 0 {
        // 并发翻转过；视作 idempotent。
        let response = BotUnfollowResponse {
            bot_user_id,
            channel_id: existing.channel_id as u64,
            unfollowed: false,
        };
        return Ok(json!(response));
    }

    let channel_id = existing.channel_id as u64;

    // Emit server event `bot.unfollowed` — best-effort fire-and-forget
    // (spec SERVER_EVENT_DISPATCH_SPEC §6)。
    if let Some(client) = services.server_event_client.clone() {
        match ServerEvent::bot_unfollowed(
            user_id,
            bot_user_id,
            channel_id,
            USER_TYPE_BOT,
            now_ms,
        ) {
            Ok(event) => {
                tokio::spawn(async move {
                    if let Err(e) = client.send(&event).await {
                        tracing::warn!(
                            target: "bot_follow",
                            "server-event 通知失败 (bot.unfollowed): {}",
                            e
                        );
                    }
                });
            }
            Err(e) => {
                tracing::warn!(
                    target: "bot_follow",
                    "server-event payload 序列化失败 (bot.unfollowed): {}",
                    e
                );
            }
        }
    }

    tracing::info!(user_id, bot_user_id, channel_id, "bot unfollow ok");

    let response = BotUnfollowResponse {
        bot_user_id,
        channel_id,
        unfollowed: true,
    };
    Ok(json!(response))
}
