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
use crate::repository::user_device_repo::PushPreference;
use privchat_protocol::rpc::device::{
    DevicePushPreferenceResponse, DevicePushPreferenceUpdateRequest,
};
use serde_json::{json, Value};

/// `device/push/preference/get`
pub async fn handle_get(
    _body: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let preference = services
        .user_device_repo
        .as_ref()
        .get_push_preference(user_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询推送偏好失败: {}", e)))?;

    Ok(json!(DevicePushPreferenceResponse {
        show_preview: preference.show_preview,
        global_mute: preference.global_mute,
    }))
}

/// `device/push/preference/update`
///
/// 两个字段都可选：只带要改的那个。读改写在服务端完成——客户端如果得先 get 再
/// 把整份 preference 送回来，两台设备同时改不同开关时，后写的那次会把对方的改动
/// 覆盖掉。
pub async fn handle_update(
    body: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let request: DevicePushPreferenceUpdateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    if request.show_preview.is_none() && request.global_mute.is_none() {
        return Err(RpcError::validation(
            "show_preview 与 global_mute 至少要提供一个".to_string(),
        ));
    }

    let user_id = crate::rpc::get_current_user_id(&ctx)?;
    let repo = services.user_device_repo.as_ref();

    let current = repo
        .get_push_preference(user_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询推送偏好失败: {}", e)))?;

    let updated = PushPreference {
        show_preview: request.show_preview.unwrap_or(current.show_preview),
        global_mute: request.global_mute.unwrap_or(current.global_mute),
    };

    repo.set_push_preference(user_id, &updated)
        .await
        .map_err(|e| RpcError::internal(format!("保存推送偏好失败: {}", e)))?;

    tracing::debug!(
        "✅ 推送偏好已更新: user_id={}, show_preview={}, global_mute={}",
        user_id,
        updated.show_preview,
        updated.global_mute
    );

    Ok(json!(DevicePushPreferenceResponse {
        show_preview: updated.show_preview,
        global_mute: updated.global_mute,
    }))
}
