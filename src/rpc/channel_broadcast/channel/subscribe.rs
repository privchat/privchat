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
use serde_json::{json, Value};

/// 处理 订阅频道 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 订阅频道 请求: {:?}", body);

    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let channel_id_str = body
        .get("channel_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("channel_id is required".to_string()))?;
    let channel_id = channel_id_str
        .parse::<u64>()
        .map_err(|_| RpcError::validation(format!("Invalid channel_id: {}", channel_id_str)))?;

    // 订阅频道（使用 Subscriber 角色）
    match services
        .channel_service
        .join_channel(
            channel_id,
            user_id,
            Some(crate::model::channel::MemberRole::Member),
        )
        .await
    {
        Ok(_) => {
            tracing::debug!("✅ 用户 {} 成功订阅频道 {}", user_id, channel_id);
            Ok(json!({
                "status": "success",
                "message": "频道订阅成功",
                "channel_id": channel_id,
                "user_id": user_id,
                "subscribed_at": chrono::Utc::now().to_rfc3339()
            }))
        }
        Err(e) => {
            tracing::error!("❌ 订阅频道失败: {}", e);
            Err(RpcError::internal(format!("订阅频道失败: {}", e)))
        }
    }
}
