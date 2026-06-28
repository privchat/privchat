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

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::settings::{GroupSettingsData, GroupSettingsGetResponse};
use serde_json::Value;

/// 处理 获取群设置 请求
///
/// RPC: group/settings/get
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "user_id": "alice"  // 请求者ID（需验证是否为群成员）
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "group_id": "group_123",
///   "settings": {
///     "join_need_approval": false,
///     "member_can_invite": false,
///     "all_muted": false,
///     "max_members": 500,
///     "announcement": "欢迎加入群组",
///     "description": "技术交流群",
///     "created_at": "2026-01-01T00:00:00Z",
///     "updated_at": "2026-01-10T00:00:00Z"
///   }
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 获取群设置 请求: {:?}", body);

    // 解析参数
    let group_id_str = body
        .get("group_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("group_id is required".to_string()))?;
    let group_id = group_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid group_id: {}", group_id_str)))?;

    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    // 1. 获取群组信息
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 2. 验证用户是否为群成员
    if !channel.members.contains_key(&user_id) {
        return Err(RpcError::forbidden("您不是群组成员".to_string()));
    }

    // 3. 群业务策略以 DB(privchat_groups) 为真源（重启不丢）；
    //    公告/描述/成员上限/时间仍取自频道。
    let policy = services
        .channel_service
        .get_group_policy(group_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询群设置失败: {}", e)))?
        .unwrap_or_default();
    let s = channel.settings.as_ref();
    let settings = GroupSettingsData {
        // 兼容旧字段：join_need_approval 由 join_policy==1（需审核）派生
        join_need_approval: policy.join_policy == 1,
        member_can_invite: policy.allow_member_invite,
        allow_member_add_friend: policy.allow_member_add_friend,
        allow_search: policy.allow_search,
        join_policy: policy.join_policy as u8,
        all_muted: policy.all_muted,
        max_members: s
            .and_then(|s| s.max_members.map(|m| m as usize))
            .or(channel.metadata.max_members)
            .unwrap_or(500),
        announcement: channel.metadata.announcement.clone(),
        description: channel.metadata.description.clone(),
        created_at: channel.created_at.timestamp_millis() as u64,
        updated_at: channel.updated_at.timestamp_millis() as u64,
    };

    tracing::debug!("✅ 获取群设置成功: group_id={}", group_id);

    let response = GroupSettingsGetResponse { group_id, settings };
    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
