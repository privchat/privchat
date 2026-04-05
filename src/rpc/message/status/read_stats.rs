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

use super::policy::{ensure_read_stats_allowed, parse_read_receipt_mode};
use crate::repository::message_repo::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 查询群消息已读统计 请求（不包含用户列表，性能更好）
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理查询群消息已读统计请求: {:?}", body);

    // 解析参数（所有ID必须是u64类型）
    let message_id = body
        .get("message_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("message_id is required (must be u64)".to_string()))?;

    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("channel_id is required (must be u64)".to_string()))?;
    let receipt_mode = parse_read_receipt_mode(&body)?;
    ensure_read_stats_allowed(receipt_mode)?;

    // 获取频道信息（用于获取成员总数）
    let channel = services
        .channel_service
        .get_channel(&channel_id)
        .await
        .map_err(|e| RpcError::not_found(format!("频道不存在: {}", e)))?;

    let total_members = channel.members.len() as u32;
    let member_ids: Vec<u64> = channel.members.keys().copied().collect();

    let message = services
        .message_repository
        .find_by_id(message_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询消息失败: {}", e)))?
        .ok_or_else(|| RpcError::not_found(format!("消息不存在: {}", message_id)))?;
    if message.channel_id != channel_id {
        return Err(RpcError::validation(
            "message_id 与 channel_id 不匹配".to_string(),
        ));
    }
    let message_pts = message.pts.unwrap_or(0).max(0) as u64;

    let read_count = services
        .read_state_service
        .count_read_members_by_message_pts(channel_id, message_pts, &member_ids)
        .await
        .map_err(|e| RpcError::internal(format!("查询已读统计失败: {}", e)))?;
    tracing::debug!(
        "✅ 查询已读统计成功(投影): message_id={} channel_id={} message_pts={} 已读 {}/{}",
        message_id,
        channel_id,
        message_pts,
        read_count,
        total_members
    );
    Ok(json!({
        "message_id": message_id,
        "channel_id": channel_id,
        "total_count": total_members,
        "read_count": read_count,
        "unread_count": total_members.saturating_sub(read_count)
    }))
}
