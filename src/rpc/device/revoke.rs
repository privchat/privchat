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

use crate::rpc::RpcServiceContext;
use crate::rpc::error::{RpcError, RpcResult};
use serde_json::{json, Value};
use tracing::{debug, info};

/// RPC: device/revoke - 踢出指定设备（撤销其 token）
/// 
/// 请求:
/// ```json
/// {
///   "device_id": "device-uuid-xxx"
/// }
/// ```
/// 
/// 响应:
/// ```json
/// {
///   "success": true,
///   "message": "设备已踢出"
/// }
/// ```
pub async fn handle(ctx: &RpcServiceContext, params: Value) -> RpcResult<Value> {
    tracing::debug!("RPC device/revoke 请求: {:?}", params);
    
    // 1. 提取设备ID
    let device_id = params
        .get("device_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation(
            "缺少 device_id 参数".to_string()
        ))?;
    
    // 2. 撤销设备
    ctx.token_revocation_service
        .revoke_device(device_id)
        .await
        .map_err(|e| RpcError::internal(e.to_string()))?;
    
    tracing::debug!("✅ device/revoke 成功: device_id={}", device_id);
    
    Ok(json!({
        "success": true,
        "message": "设备已踢出"
    }))
}

