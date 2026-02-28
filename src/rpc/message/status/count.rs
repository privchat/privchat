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

use crate::rpc::error::RpcResult;
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 消息计数 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 消息计数 请求: {:?}", body);

    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    // 兼容数字和字符串两种 channel_id 格式
    let channel_id: Option<u64> = body.get("channel_id").and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    });

    if let Some(ch_id) = channel_id {
        // 从 unread_count_service 读取该频道的未读计数
        let unread = match services.unread_count_service.get(user_id, ch_id).await {
            Ok(count) => count as i32,
            Err(_) => 0,
        };

        Ok(json!({
            "unread_count": unread,
            "channel_id": ch_id.to_string(),
        }))
    } else {
        // 返回用户的总未读计数（所有频道加总）
        let total = match services.unread_count_service.get_all(user_id).await {
            Ok(counts) => counts.values().sum::<u64>() as i32,
            Err(_) => 0,
        };

        Ok(json!({
            "unread_count": total,
        }))
    }
}
