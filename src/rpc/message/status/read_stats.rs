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

use super::policy::{
    authorize_read_detail, ensure_read_stats_allowed, read_detail_retention_days,
    resolve_read_receipt_mode,
};
use crate::repository::message_repo::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

fn u64_field(body: &Value, key: &str) -> RpcResult<u64> {
    body.get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .ok_or_else(|| RpcError::validation(format!("{} is required (must be u64)", key)))
}

/// 查询某条群消息的已读**人数**（READ_STATUS_SPEC §6.5）。
///
/// 与名单共用 [`authorize_read_detail`]，两个接口的口径必须一致——各算各的正是
/// 人数与名单对不上的来源。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let message_id = u64_field(&body, "message_id")?;
    let channel_id = u64_field(&body, "channel_id")?;

    ensure_read_stats_allowed(resolve_read_receipt_mode())?;

    let requester_id = crate::rpc::get_current_user_id(&ctx)?;
    let message = services
        .message_repository
        .find_by_id(message_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询消息失败: {}", e)))?
        .ok_or_else(|| RpcError::not_found(format!("消息不存在: {}", message_id)))?;

    let channel = services
        .channel_service
        .get_channel(&channel_id)
        .await
        .map_err(|e| RpcError::not_found(format!("频道不存在: {}", e)))?;

    let expires_at = authorize_read_detail(requester_id, &message, channel_id, &channel)?;

    let recipient_ids: Vec<u64> = channel
        .get_member_ids()
        .into_iter()
        .filter(|id| *id != message.sender_id)
        .collect();

    let message_pts = message.pts.unwrap_or(0).max(0) as u64;
    // 人数只来自阅读数据，不掺资料加载结果；与名单接口同一个 COUNT，口径不会分叉。
    let read_count = services
        .read_state_service
        .count_read_members_by_message_pts(channel_id, message_pts, &recipient_ids)
        .await
        .map_err(|e| RpcError::internal(format!("查询已读统计失败: {}", e)))?;
    let recipient_count = recipient_ids.len() as u32;
    Ok(json!({
        "message_id": message_id,
        "channel_id": channel_id,
        "recipient_count": recipient_count,
        "read_count": read_count,
        "unread_count": recipient_count.saturating_sub(read_count),
        "detail_expires_at": expires_at.timestamp_millis(),
        "retention_days": read_detail_retention_days(),
    }))
}
