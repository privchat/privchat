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
use privchat_protocol::rpc::group::settings::{
    GroupSettingsUpdateRequest, GroupSettingsUpdateResponse,
};
use serde_json::Value;

/// 处理 更新群设置 请求
///
/// RPC: `group/settings/update`
///
/// 请求/响应均使用 privchat-protocol 的 typed 类型
/// ([`GroupSettingsUpdateRequest`] / [`GroupSettingsUpdateResponse`])，不手写 JSON 解析。
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 更新群设置 请求: {:?}", body);

    // 1. 反序列化为协议类型（typed）
    let request: GroupSettingsUpdateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    let group_id = request.group_id;

    // operator_id 以连接上下文为准，避免客户端伪造越权
    let operator_id = crate::rpc::get_current_user_id(&ctx).unwrap_or(request.operator_id);
    let patch = &request.settings;

    // 2. 获取群组信息
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 3. 验证操作者权限（仅群主可以修改群设置）
    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner
    ) {
        return Err(RpcError::forbidden("只有群主可以修改群设置".to_string()));
    }

    // 4. 从 typed patch 取各可选项
    let join_need_approval = patch.join_need_approval;
    let member_can_invite = patch.member_can_invite;
    let all_muted = patch.all_muted;
    let allow_member_add_friend = patch.allow_member_add_friend;
    let allow_search = patch.allow_search;
    let join_policy = patch.join_policy;
    let max_members = patch.max_members;
    let announcement = patch.announcement.clone();
    let description = patch.description.clone();

    // 5. 更新群设置
    let mut update_count = 0;

    // 5.0. 群业务策略先落库（DB 为真源），再由下方 setter 同步内存缓存。
    //      仅当存在 policy 字段变更时才写库，避免无谓 UPDATE。
    // 全员禁言变化要发系统灰条:先取旧值判断是否真的翻转
    let prev_all_muted = if all_muted.is_some() {
        services
            .channel_service
            .get_group_policy(group_id)
            .await
            .ok()
            .flatten()
            .map(|p| p.all_muted)
    } else {
        None
    };

    if allow_search.is_some()
        || join_policy.is_some()
        || member_can_invite.is_some()
        || allow_member_add_friend.is_some()
        || all_muted.is_some()
    {
        services
            .channel_service
            .update_group_policy(
                group_id,
                allow_search,
                join_policy.map(|v| v as i16),
                member_can_invite,
                allow_member_add_friend,
                all_muted,
            )
            .await
            .map_err(|e| RpcError::internal(format!("群设置落库失败: {}", e)))?;
        tracing::debug!("✅ 群设置已落库: group_id={}", group_id);
    }

    // 5.0.1 全员禁言开/关系统灰条(template + refs[操作人],三端共用同一渲染)
    if let Some(muted) = all_muted {
        if prev_all_muted != Some(muted) {
            let operator_name = services
                .user_service
                .find_by_id(operator_id)
                .await
                .ok()
                .flatten()
                .and_then(|u| u.display_name.or(u.username))
                .unwrap_or_else(|| operator_id.to_string());
            let sys_payload = serde_json::json!({
                "message_type": "system",
                "template": if muted { "system.group_mute_all_on" } else { "system.group_mute_all_off" },
                "refs": [{
                    "type": "user",
                    "target_id": operator_id.to_string(),
                    "text": operator_name,
                }],
            });
            if let Err(e) = services
                .message_service
                .send_group_system_message(group_id, sys_payload.to_string(), serde_json::json!({}))
                .await
            {
                tracing::warn!("⚠️ 写入全员禁言系统消息失败 group_id={}: {}", group_id, e);
            }
        }
    }

    // 4.1. 更新描述
    if let Some(desc) = description {
        services
            .channel_service
            .update_channel_metadata(
                &group_id,
                None, // name不变
                Some(desc),
                None, // avatar_url不变
            )
            .await
            .map_err(|e| RpcError::internal(format!("更新群描述失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群描述成功");
    }

    // 4.2. 更新群公告
    if let Some(ann) = announcement {
        services
            .channel_service
            .set_channel_announcement(&group_id, Some(ann))
            .await
            .map_err(|e| RpcError::internal(format!("更新群公告失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群公告成功");
    }

    // 4.3. 更新全员禁言
    if let Some(muted) = all_muted {
        services
            .channel_service
            .set_channel_all_muted(&group_id, muted)
            .await
            .map_err(|e| RpcError::internal(format!("更新全员禁言失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新全员禁言成功: {}", muted);
    }

    // 4.4. 更新加群审批
    if let Some(approval) = join_need_approval {
        services
            .channel_service
            .set_channel_join_approval(&group_id, approval)
            .await
            .map_err(|e| RpcError::internal(format!("更新加群审批失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新加群审批成功: {}", approval);
    }

    // 4.5. 更新成员邀请权限
    if let Some(invite) = member_can_invite {
        services
            .channel_service
            .set_channel_member_invite(&group_id, invite)
            .await
            .map_err(|e| RpcError::internal(format!("更新成员邀请权限失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新成员邀请权限成功: {}", invite);
    }

    // 4.5.1. 更新"群成员私自加好友"开关（P0-5）
    if let Some(allow) = allow_member_add_friend {
        services
            .channel_service
            .set_channel_allow_member_add_friend(&group_id, allow)
            .await
            .map_err(|e| RpcError::internal(format!("更新加好友权限失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群成员加好友权限成功: {}", allow);
    }

    // 4.5.2. 更新"群可被搜索"开关（P0-4）
    if let Some(allow) = allow_search {
        services
            .channel_service
            .set_channel_allow_search(&group_id, allow)
            .await
            .map_err(|e| RpcError::internal(format!("更新群搜索开关失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新群可被搜索成功: {}", allow);
    }

    // 4.5.3. 更新"加入策略"（P0-4）
    if let Some(policy) = join_policy {
        services
            .channel_service
            .set_channel_join_policy(&group_id, policy)
            .await
            .map_err(|e| RpcError::internal(format!("更新加入策略失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新加入策略成功: {}", policy);
    }

    // 4.6. 更新成员上限
    if let Some(max) = max_members {
        services
            .channel_service
            .set_channel_max_members(&group_id, max)
            .await
            .map_err(|e| RpcError::internal(format!("更新成员上限失败: {}", e)))?;
        update_count += 1;
        tracing::debug!("✅ 更新成员上限成功: {}", max);
    }

    tracing::debug!(
        "✅ 群设置更新成功: group_id={}, 更新项数={}",
        group_id,
        update_count
    );

    // 6. 构建 typed 响应并序列化
    let response = GroupSettingsUpdateResponse {
        success: true,
        group_id: group_id.to_string(),
        message: format!("群设置更新成功，共更新 {} 项", update_count),
        updated_count: update_count as u32,
        updated_at: chrono::Utc::now().timestamp_millis() as u64,
    };

    serde_json::to_value(response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}
