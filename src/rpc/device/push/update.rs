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
use crate::rpc::RpcContext;
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::device::{DevicePushUpdateRequest, DevicePushUpdateResponse};
use serde_json::{json, Value};

/// RPC Handler: device/push/update
///
/// 更新设备推送状态（前台/后台切换时调用）
pub async fn handle(body: Value, services: RpcServiceContext, ctx: RpcContext) -> RpcResult<Value> {
    tracing::debug!("RPC device/push/update 请求: {:?}", body);

    // 1. 解析请求
    let request: DevicePushUpdateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 2. 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    // 3. 更新设备状态
    services
        .user_device_repo
        .as_ref()
        .update_device_push_state(
            user_id,
            &request.device_id,
            request.apns_armed,
            request.push_token.as_deref(),
            request.vendor.as_deref(),
        )
        .await
        .map_err(|e| RpcError::internal(format!("更新设备推送状态失败: {}", e)))?;

    tracing::debug!(
        "✅ 设备推送状态已更新: user_id={}, device_id={}, apns_armed={}",
        user_id,
        request.device_id,
        request.apns_armed
    );

    // 4. 检查用户级别推送状态
    let user_push_enabled = services
        .user_device_repo
        .as_ref()
        .check_user_push_enabled(user_id)
        .await
        .map_err(|e| RpcError::internal(format!("检查用户推送状态失败: {}", e)))?;

    // 5. 返回响应
    let response = DevicePushUpdateResponse {
        device_id: request.device_id,
        apns_armed: request.apns_armed,
        user_push_enabled,
    };

    Ok(json!(response))
}
