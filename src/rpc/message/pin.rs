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

//! 群消息置顶 RPC handler（P1）
//!
//! - `message/pin`：群主/管理员置顶 / 取消置顶群内消息
//! - `message/pin/list`：获取群置顶消息列表
//!
//! 请求/响应均使用 privchat-protocol 的 typed 类型，不手写 JSON。
//! 持久化表：`privchat_group_pinned_messages`（migration 003）。

use crate::model::channel::{Channel, MemberRole};
use crate::repository::MessageRepository;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::message::pin::{
    MessagePinListRequest, MessagePinListResponse, MessagePinRequest, MessagePinResponse,
    PinnedMessageItem,
};
use serde_json::Value;

/// 校验操作者是该群的群主 / 管理员；返回群对应的 channel 与操作者角色。
/// 返回错误则无权操作。
async fn ensure_group_manager(
    services: &RpcServiceContext,
    group_id: u64,
    operator_id: u64,
) -> RpcResult<(Channel, MemberRole)> {
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 该 channel 必须确实是本群的群聊频道（防止 channel_id/group_id ID 空间漂移）。
    if channel.group_id != Some(group_id) {
        return Err(RpcError::not_found("群组不存在".to_string()));
    }

    let member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    let role = member.role;
    if !matches!(role, MemberRole::Owner | MemberRole::Admin) {
        return Err(RpcError::forbidden(
            "只有群主或管理员可以置顶消息".to_string(),
        ));
    }
    Ok((channel, role))
}

/// 处理 置顶 / 取消置顶 群消息：`message/pin`
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 群消息置顶 请求: {:?}", body);

    // 1. typed 反序列化
    let mut request: MessagePinRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    // operator 以连接上下文为准
    request.operator_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let message_id = request.message_id;
    let operator_id = request.operator_id;

    // 2. 权限校验：仅群主/管理员，并取回群对应的 channel + 操作者角色
    let (channel, operator_role) = ensure_group_manager(&services, group_id, operator_id).await?;

    // 3. 三方一致性校验（防越权置顶外群消息）：
    //    request.channel_id 必须就是本群的群聊 channel，message 也必须属于该 channel。
    if request.channel_id != channel.id {
        return Err(RpcError::validation("channel_id 不属于该群".to_string()));
    }

    // 4. 校验消息存在且属于本群对应的 channel
    let message = services
        .message_repository
        .find_by_id(message_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询消息失败: {}", e)))?
        .ok_or_else(|| RpcError::not_found("消息不存在".to_string()))?;

    if message.revoked {
        return Err(RpcError::validation("消息已被撤回，无法置顶".to_string()));
    }
    if message.channel_id != channel.id {
        return Err(RpcError::validation(
            "消息不属于该群（channel_id 不匹配）".to_string(),
        ));
    }

    let pool = services.channel_service.pool();
    let now = chrono::Utc::now().timestamp_millis();

    if request.pinned {
        // 4a. 置顶：幂等 upsert
        sqlx::query(
            "INSERT INTO privchat_group_pinned_messages \
             (group_id, message_id, channel_id, pinned_by, pinned_at) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (group_id, message_id) DO NOTHING",
        )
        .bind(group_id as i64)
        .bind(message_id as i64)
        .bind(message.channel_id as i64)
        .bind(operator_id as i64)
        .bind(now)
        .execute(pool)
        .await
        .map_err(|e| RpcError::internal(format!("置顶写入失败: {}", e)))?;

        let response = MessagePinResponse {
            success: true,
            group_id,
            message_id,
            pinned: true,
            pinned_at: Some(now as u64),
            pinned_by: Some(operator_id),
        };
        serde_json::to_value(response)
            .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
    } else {
        // 4b. 取消置顶。
        //   权限规则：群主可取消任意置顶；管理员可取消自己或其它管理员的置顶，
        //   但不能取消群主的置顶。
        let existing: Option<(i64,)> = sqlx::query_as(
            "SELECT pinned_by FROM privchat_group_pinned_messages \
             WHERE group_id = $1 AND message_id = $2",
        )
        .bind(group_id as i64)
        .bind(message_id as i64)
        .fetch_optional(pool)
        .await
        .map_err(|e| RpcError::internal(format!("查询置顶记录失败: {}", e)))?;

        if let Some((pinned_by,)) = existing {
            let pinned_by = pinned_by as u64;
            // 操作者非群主、且要取消的不是自己置顶的 → 检查原置顶者是否群主
            if !matches!(operator_role, MemberRole::Owner) && pinned_by != operator_id {
                let pinner_is_owner = channel
                    .members
                    .get(&pinned_by)
                    .map(|m| matches!(m.role, MemberRole::Owner))
                    .unwrap_or(false);
                if pinner_is_owner {
                    return Err(RpcError::forbidden(
                        "管理员不能取消群主置顶的消息".to_string(),
                    ));
                }
            }
        }

        sqlx::query(
            "DELETE FROM privchat_group_pinned_messages WHERE group_id = $1 AND message_id = $2",
        )
        .bind(group_id as i64)
        .bind(message_id as i64)
        .execute(pool)
        .await
        .map_err(|e| RpcError::internal(format!("取消置顶失败: {}", e)))?;

        let response = MessagePinResponse {
            success: true,
            group_id,
            message_id,
            pinned: false,
            pinned_at: None,
            pinned_by: None,
        };
        serde_json::to_value(response)
            .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
    }
}

/// 处理 获取群置顶消息列表：`message/pin/list`
pub async fn handle_list(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 群置顶消息列表 请求: {:?}", body);

    let mut request: MessagePinListRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;

    // 群成员可查看置顶列表
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;
    if !channel.members.contains_key(&request.user_id) {
        return Err(RpcError::forbidden("您不是群组成员".to_string()));
    }

    let pool = services.channel_service.pool();
    let rows: Vec<(i64, i64, i64, i64)> = sqlx::query_as(
        "SELECT message_id, channel_id, pinned_by, pinned_at \
         FROM privchat_group_pinned_messages \
         WHERE group_id = $1 \
         ORDER BY pinned_at DESC",
    )
    .bind(group_id as i64)
    .fetch_all(pool)
    .await
    .map_err(|e| RpcError::internal(format!("查询置顶列表失败: {}", e)))?;

    let items = rows
        .into_iter()
        .map(
            |(message_id, channel_id, pinned_by, pinned_at)| PinnedMessageItem {
                message_id: message_id as u64,
                channel_id: channel_id as u64,
                pinned_by: pinned_by as u64,
                pinned_at: pinned_at as u64,
            },
        )
        .collect();

    let response = MessagePinListResponse { group_id, items };
    serde_json::to_value(response).map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
