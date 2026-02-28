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

//! 更新用户设置（ENTITY_SYNC_V1 user_settings）
//!
//! RPC: account/settings/update
//! 支持单条 { "key": "theme", "value": "dark" } 或批量 { "settings": { "theme": "dark", "language": "zh-CN" } }

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcContext;
use crate::rpc::{get_current_user_id, RpcServiceContext};
use serde_json::{json, Value};

/// 处理 account/settings/update 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 account/settings/update 请求");

    let user_id = get_current_user_id(&ctx)?;

    let version = if let Some(settings) = body.get("settings").and_then(|v| v.as_object()) {
        let map: std::collections::HashMap<String, Value> = settings
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if map.is_empty() {
            return Err(RpcError::validation("settings 不能为空".to_string()));
        }
        services
            .user_settings_repo
            .set_batch(user_id, &map)
            .await
            .map_err(|e| RpcError::internal(format!("批量更新设置失败: {}", e)))?
    } else if let (Some(key), Some(value)) =
        (body.get("key").and_then(|v| v.as_str()), body.get("value"))
    {
        services
            .user_settings_repo
            .set(user_id, key, value)
            .await
            .map_err(|e| RpcError::internal(format!("更新设置失败: {}", e)))?
    } else {
        return Err(RpcError::validation(
            "请提供 \"key\" + \"value\" 或 \"settings\": { key: value, ... }".to_string(),
        ));
    };

    Ok(json!({
        "ok": true,
        "version": version
    }))
}
