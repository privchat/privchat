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

/// 处理"踢出其他设备"请求
///
/// 用户可以从当前设备踢出所有其他登录的设备。
///
/// 实现原理：
/// 1. 更新设备状态：session_version + 1, session_state = KICKED
/// 2. 断开连接：connection_manager.disconnect_device()
/// 3. Token 失效：旧 token 的 version < 新 version，验证失败
///
/// 请求示例：
/// ```json
/// {}  // 无需参数，自动从认证会话中获取当前用户和设备
/// ```
///
/// 响应示例：
/// ```json
/// {
///   "success": true,
///   "message": "已踢出 2 个设备",
///   "kicked_devices": [
///     {
///       "device_id": "uuid",
///       "device_name": "iPhone 15 Pro",
///       "device_type": "ios",
///       "old_version": 1,
///       "new_version": 2
///     }
///   ]
/// }
/// ```
pub async fn handle(
    _body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理踢出其他设备请求");

    // 1. 从 RpcContext 获取当前用户和设备ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let current_device_id = ctx
        .device_id
        .as_ref()
        .ok_or_else(|| RpcError::validation("缺少设备ID".to_string()))?;

    tracing::debug!(
        "踢出其他设备: user={}, current_device={}",
        user_id,
        current_device_id
    );

    // 2. 踢出其他设备（使用数据库版本）
    let kicked_devices = services
        .device_manager_db
        .kick_other_devices(user_id, current_device_id, "user_requested")
        .await
        .map_err(|e| RpcError::internal(format!("踢出设备失败: {}", e)))?;

    // 3. 断开所有其他设备的连接（✨ 新增）
    if let Err(e) = services
        .connection_manager
        .disconnect_other_devices(user_id, current_device_id)
        .await
    {
        tracing::warn!(
            "⚠️ 批量断开设备连接失败（部分设备可能未在线）: user={}, error={}",
            user_id,
            e
        );
    }

    let count = kicked_devices.len();

    tracing::debug!("✅ 已踢出 {} 个设备: user={}", count, user_id);

    // 3. 返回结果
    Ok(json!({
        "success": true,
        "message": format!("已踢出 {} 个设备", count),
        "kicked_devices": kicked_devices,
    }))
}
