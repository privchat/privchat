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
use crate::rpc::{get_current_user_id, RpcContext, RpcServiceContext};
use serde_json::{json, Value};

fn parse_session_id(raw: &str) -> RpcResult<msgtrans::SessionId> {
    let id = raw
        .strip_prefix("session-")
        .unwrap_or(raw)
        .parse::<u64>()
        .map_err(|e| RpcError::validation(format!("invalid session_id '{}': {}", raw, e)))?;
    Ok(msgtrans::SessionId::from(id))
}

pub async fn handle(
    _body: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = get_current_user_id(&ctx)?;
    let session_id_raw = ctx
        .session_id
        .as_ref()
        .ok_or_else(|| RpcError::unauthorized("missing session_id".to_string()))?;
    let session_id = parse_session_id(session_id_raw)?;

    let removed = services
        .connection_manager
        .unregister_connection(session_id)
        .await
        .map_err(|e| RpcError::internal(format!("unregister connection failed: {}", e)))?;

    // 获取设备信息用于后续清理
    let mut cleanup_device_id = ctx.device_id.clone();
    if cleanup_device_id.is_none() {
        if let Some((removed_uid, removed_device_id)) = removed {
            if removed_uid == user_id {
                cleanup_device_id = Some(removed_device_id);
            }
        }
    }

    // 通知 Presence 服务用户下线（修复：之前缺少此调用，导致订阅者收不到离线状态）
    if let Some(device_id) = &cleanup_device_id {
        if let Err(e) = services
            .presence_service
            .on_device_disconnected(user_id, device_id)
            .await
        {
            // 记录警告但不阻止 logout 流程
            tracing::warn!(
                "⚠️ LogoutHandler: 更新 Presence 下线失败: user_id={}, error={}",
                user_id,
                e
            );
        }
    }

    // 解绑认证会话
    services
        .auth_session_manager
        .unbind_session(&session_id)
        .await;

    // 清理消息路由和设备状态
    if let Some(device_id) = cleanup_device_id {
        let _ = services
            .message_router
            .register_device_offline(&user_id, &device_id, Some(session_id_raw))
            .await;

        // 递增会话版本号（AUTH_SPEC 2.5：主动登出应递增 session_version）
        if let Err(e) = services
            .device_manager_db
            .increment_session_version(user_id, &device_id, "user_logout")
            .await
        {
            tracing::warn!(
                "⚠️ LogoutHandler: 递增 session_version 失败: user_id={}, error={}",
                user_id,
                e
            );
        }
    }

    Ok(json!({
        "success": true,
        "message": "logout success"
    }))
}
