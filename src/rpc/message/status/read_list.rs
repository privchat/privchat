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
use crate::rpc::{helpers, RpcServiceContext};
use serde_json::{json, Value};

/// 处理 查询群消息已读列表 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理查询群消息已读列表请求: {:?}", body);

    // 解析参数
    let message_id_str = body
        .get("message_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("message_id is required".to_string()))?;
    let message_id = message_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid message_id: {}", message_id_str)))?;

    let channel_id_str = body
        .get("channel_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("channel_id is required".to_string()))?;
    let channel_id = channel_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid channel_id: {}", channel_id_str)))?;

    // 获取频道信息（用于获取成员总数）
    let channel = services
        .channel_service
        .get_channel(&channel_id)
        .await
        .map_err(|e| RpcError::not_found(format!("频道不存在: {}", e)))?;

    let total_members = channel.members.len() as u32;

    // 获取已读列表
    match services
        .read_receipt_service
        .get_group_read_list(&message_id, &channel_id, total_members)
        .await
    {
        Ok(stats) => {
            // 获取已读用户的详细信息（从数据库读取）
            let mut read_users_info = Vec::new();
            if let Some(read_users) = &stats.read_users {
                for user_id in read_users {
                    // 获取用户资料
                    if let Ok(Some(profile)) = helpers::get_user_profile_with_fallback(
                        *user_id,
                        &services.user_repository,
                        &services.cache_manager,
                    )
                    .await
                    {
                        // 获取已读时间
                        let read_at = services
                            .read_receipt_service
                            .get_read_time(&message_id, &user_id)
                            .await
                            .ok()
                            .flatten()
                            .unwrap_or_else(|| chrono::Utc::now());

                        read_users_info.push(json!({
                            "user_id": profile.user_id,
                            "username": profile.username,
                            "nickname": profile.nickname,
                            "avatar_url": profile.avatar_url,
                            "read_at": read_at.to_rfc3339()
                        }));
                    }
                }
            }

            tracing::debug!(
                "✅ 查询已读列表成功: 消息 {} 在频道 {}，已读 {}/{}",
                message_id,
                channel_id,
                stats.read_count,
                total_members
            );
            Ok(json!({
                "message_id": message_id_str,
                "channel_id": channel_id_str,
                "total_members": total_members,
                "read_count": stats.read_count,
                "unread_count": total_members - stats.read_count,
                "read_list": read_users_info  // 修改为 read_list 以匹配客户端期望
            }))
        }
        Err(e) => {
            tracing::error!("❌ 查询已读列表失败: {}", e);
            Err(RpcError::internal(format!("查询已读列表失败: {}", e)))
        }
    }
}
