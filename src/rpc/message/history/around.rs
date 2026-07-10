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
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! `message/history/around` — 搜索命中/消息定位的上下文拉取
//! （MESSAGE_HISTORY_AND_SEARCH spec §5，Telegram jump-to-message 体验）。
//!
//! 语义：
//! - 成员鉴权走 `ensure_channel_visible`（非成员/频道不存在统一 not_found）；
//! - anchor 不存在 / 不属于该 channel / 已软删 / 已撤回 → 统一 not_found
//!   （不区分原因，防存在性泄露；用户可能拿旧搜索结果或本地缓存的 id 点击）；
//! - before/after 两侧严格 (created_at, message_id) 元组 keyset；
//! - 上下文内的撤回消息保留占位（content 空 + revoked 标记），不消失；
//! - 返回的是**完整消息**（与 message/history/get 同一 JSON 视图）——SDK 允许
//!   回填本地缓存；search 的 snippet 投影则不允许（边界见 spec §6）。

use crate::repository::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

const DEFAULT_SIDE_LIMIT: u64 = 20;
const MAX_SIDE_LIMIT: u64 = 50;

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("channel_id is required (must be u64)".to_string()))?;
    let message_id = body
        .get("message_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("message_id is required (must be u64)".to_string()))?;
    let before_limit = body
        .get("before_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_SIDE_LIMIT)
        .clamp(0, MAX_SIDE_LIMIT) as i64;
    let after_limit = body
        .get("after_limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_SIDE_LIMIT)
        .clamp(0, MAX_SIDE_LIMIT) as i64;

    // 成员鉴权（spec §3：读路径统一 not_found）
    crate::rpc::ensure_channel_visible(services.channel_service.as_ref(), channel_id, user_id)
        .await?;

    // anchor 加载与可见性：不存在/跨频道/软删/撤回 → 统一 not_found
    let anchor = MessageRepository::find_by_id(services.message_repository.as_ref(), message_id)
        .await
        .map_err(|e| RpcError::internal(format!("load anchor failed: {}", e)))?
        .filter(|m| m.channel_id == channel_id && !m.deleted && !m.revoked)
        .ok_or_else(|| RpcError::not_found(format!("消息不存在: {}", message_id)))?;

    let anchor_ts = anchor.created_at.timestamp_millis();
    let repo = services.message_repository.as_ref();

    let (before, after) = tokio::try_join!(
        repo.list_context_before(channel_id, anchor_ts, anchor.message_id as i64, before_limit),
        repo.list_context_after(channel_id, anchor_ts, anchor.message_id as i64, after_limit),
    )
    .map_err(|e| RpcError::internal(format!("load context failed: {}", e)))?;

    let has_more_before = before.len() as i64 == before_limit && before_limit > 0;
    let has_more_after = after.len() as i64 == after_limit && after_limit > 0;

    // before 由 DESC 反转为 ASC，整体时间线 = before(ASC) + anchor + after(ASC)
    let before_views: Vec<Value> = before
        .iter()
        .rev()
        .map(super::message_view_json)
        .collect();
    let after_views: Vec<Value> = after.iter().map(super::message_view_json).collect();

    tracing::debug!(
        "🎯 message/history/around: user={}, channel={}, anchor={}, before={}, after={}",
        user_id,
        channel_id,
        message_id,
        before_views.len(),
        after_views.len()
    );

    Ok(json!({
        "before_messages": before_views,
        "anchor_message": super::message_view_json(&anchor),
        "after_messages": after_views,
        "has_more_before": has_more_before,
        "has_more_after": has_more_after,
    }))
}
