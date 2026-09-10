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
    authorize_read_detail, ensure_read_list_allowed, read_detail_retention_days,
    resolve_read_receipt_mode,
};
use crate::repository::message_repo::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use serde_json::{json, Value};

/// 名单分页：默认 30、上限 100（READ_STATUS_SPEC §6.5.7）。
const DEFAULT_PAGE_SIZE: u32 = 30;
const MAX_PAGE_SIZE: u32 = 100;

fn u64_field(body: &Value, key: &str) -> RpcResult<u64> {
    body.get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
        })
        .ok_or_else(|| RpcError::validation(format!("{} is required (must be u64)", key)))
}

/// 查询某条群消息的已读**名单**（READ_STATUS_SPEC §6.5）。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let message_id = u64_field(&body, "message_id")?;
    let channel_id = u64_field(&body, "channel_id")?;
    // 键集分页：上一页最后一个 user_id。首页传 0/不传。
    let after_user_id = body.get("after_user_id").and_then(|v| v.as_u64()).unwrap_or(0);
    let limit = body
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| (v as u32).clamp(1, MAX_PAGE_SIZE))
        .unwrap_or(DEFAULT_PAGE_SIZE);

    // 🔴 模式来自服务端，不从请求体解析。
    ensure_read_list_allowed(resolve_read_receipt_mode())?;

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

    // 发送者判定 + 当前访问权 + 撤回 + 窗口，全在这一个入口里（§6.5.5）。
    // 翻页途中过期会在这里被拒。
    let expires_at = authorize_read_detail(requester_id, &message, channel_id, &channel)?;

    let message_pts = message.pts.unwrap_or(0).max(0) as u64;
    // 🔴 「**发送时**有权接收的其他用户」，不是当前成员表（§6.5.3）。
    // 用当前成员表会让退群者消失、后加入者混入。发送者自己在查询里就排除了。
    let recipient_ids = services
        .read_state_service
        .recipients_at_send_time(channel_id, message_pts, message.sender_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询发送时收件人失败: {}", e)))?;
    // 🔴 键集分页在数据库里做，不是"全量查出来再内存 skip/take"——后者既把整份名单
    // 读进内存，又会在名单增长时重复返回。
    let page = services
        .read_state_service
        .page_read_members_by_message_pts(
            channel_id,
            message_pts,
            &recipient_ids,
            after_user_id,
            limit,
        )
        .await
        .map_err(|e| RpcError::internal(format!("查询已读列表失败: {}", e)))?;
    let read_count = services
        .read_state_service
        .count_read_members_by_message_pts(channel_id, message_pts, &recipient_ids)
        .await
        .map_err(|e| RpcError::internal(format!("查询已读人数失败: {}", e)))?;

    // 🔴 阅读事实与资料分离：资料拉不到就降级显示，不把人从名单里丢掉，
    // 也绝不用"资料成功条数"当人数——那会让人数随资料服务抖动。
    let mut read_list = Vec::with_capacity(page.len());
    for reader in page.iter() {
        let profile = helpers::get_user_profile_with_fallback(
            reader.user_id,
            &services.user_repository,
            &services.cache_manager,
        )
        .await
        .ok()
        .flatten();
        read_list.push(json!({
            "user_id": reader.user_id,
            // PROFILE_VISIBILITY：公开投影，不回他人 username。
            "username": String::new(),
            "nickname": profile.as_ref().map(|p| p.nickname.clone()).unwrap_or_default(),
            "avatar_url": profile.as_ref().and_then(|p| p.avatar_url.clone()),
            "profile_loaded": profile.is_some(),
        }));
    }

    Ok(json!({
        "message_id": message_id,
        "channel_id": channel_id,
        "recipient_count": recipient_ids.len() as u32,
        "read_count": read_count,
        "limit": limit,
        "next_after_user_id": page.last().map(|r| r.user_id),
        "has_more": page.len() as u32 == limit,
        "detail_expires_at": expires_at.timestamp_millis(),
        "retention_days": read_detail_retention_days(),
        "read_list": read_list,
    }))
}
