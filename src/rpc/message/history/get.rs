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

use crate::repository::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 获取历史消息 请求
///
/// ✨ 从数据库获取完整的历史消息（不再使用内存缓存）
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    // 解析参数
    let channel_id = body
        .get("channel_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("channel_id is required (must be u64)".to_string()))?;

    // MESSAGE_HISTORY_AND_SEARCH spec §3：读路径成员鉴权。此前 handler 拿到 ctx
    // 但从未校验，任意登录用户可拉任意会话历史（P0 越权）。非成员/频道不存在
    // 统一 not_found，不泄露频道存在性。
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    crate::rpc::ensure_channel_visible(services.channel_service.as_ref(), channel_id, user_id)
        .await?;

    let limit = body.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as i64;

    let before_message_id = body
        .get("before_server_message_id")
        .and_then(|v| v.as_u64());

    tracing::debug!(
        "🔧 从数据库获取历史消息: channel_id={}, limit={}, before_server_message_id={:?}",
        channel_id,
        limit,
        before_message_id
    );

    // ✨ 从数据库查询消息（channel_id 就是 channel_id）
    let before_created_at = if let Some(before_id) = before_message_id {
        // 如果提供了 before_message_id，先查询该消息的创建时间
        match MessageRepository::find_by_id(services.message_repository.as_ref(), before_id).await {
            Ok(Some(msg)) => Some(msg.created_at.timestamp_millis()),
            Ok(None) => {
                tracing::warn!("⚠️ before_message_id {} 不存在，忽略分页参数", before_id);
                None
            }
            Err(e) => {
                tracing::warn!("⚠️ 查询 before_message_id 失败: {}，忽略分页参数", e);
                None
            }
        }
    } else {
        None
    };

    // 从数据库查询消息列表（仓库返回 DESC 最新在先，此处反转为 ASC 便于客户端按 1→2→3 展示）
    match MessageRepository::list_by_channel(
        services.message_repository.as_ref(),
        channel_id,
        limit,
        before_created_at,
    )
    .await
    {
        Ok(messages) => {
            let mut message_list: Vec<Value> = messages
                .into_iter()
                .map(|msg| {
                    // 如果消息被撤回，清空内容（返回空字符串），客户端根据 revoked 字段显示占位符
                    let content = if msg.revoked {
                        String::new() // 清空内容，客户端根据语言显示占位符
                    } else {
                        msg.content
                    };

                    json!({
                        "message_id": msg.message_id,
                        "channel_id": msg.channel_id,  // channel_id 就是 channel_id
                        "sender_id": msg.sender_id,
                        "content": content,
                        "message_type": msg.message_type.as_str(),
                        "timestamp": msg.created_at.timestamp_millis(),
                        "message_seq": msg.pts,  // per-channel pts; clients project read-by-peer (sent vs ✓✓) by comparing this to ChannelRecord.peer_read_pts. Field name mirrors SendMessageResponse.message_seq for cross-RPC consistency.
                        "reply_to_message_id": msg.reply_to_message_id,
                        "metadata": msg.metadata,
                        "revoked": msg.revoked,  // 客户端根据此字段和语言设置显示占位符
                        "revoked_at": msg.revoked_at.map(|dt| dt.timestamp_millis()),
                        "revoked_by": msg.revoked_by
                    })
                })
                .collect();
            message_list.reverse();

            tracing::debug!(
                "✅ 从数据库获取到 {} 条历史消息（已按时间升序返回）",
                message_list.len()
            );

            Ok(json!({
                "messages": message_list,
                "total": message_list.len(),
                "has_more": message_list.len() == limit as usize
            }))
        }
        Err(e) => {
            tracing::error!("❌ 从数据库获取历史消息失败: {}", e);
            Err(RpcError::internal(format!("获取历史消息失败: {}", e)))
        }
    }
}
