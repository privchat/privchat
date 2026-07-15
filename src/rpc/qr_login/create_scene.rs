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

//! Web/PC 在 unauth RPC 连接上申请扫码登录场景（spec QR_API §5）。
//!
//! 流程：
//! 1. 解析 [`QrLoginCreateSceneRequest`]
//! 2. 从 [`crate::rpc::RpcContext::session_id`] 还原 [`msgtrans::SessionId`]
//! 3. 调 [`crate::service::qr_login_service::QrLoginService::create_scene`] 生成 scene
//! 4. 把 `scene_id ↔ session_id` 注册到 [`crate::service::QrLoginPublisher`]，
//!    覆盖该 session 之前可能存在的旧 scene
//! 5. 返回 [`QrLoginCreateSceneResponse`]
//!
//! 后续 mobile 通过 admin HTTP 触发 scan/confirm/reject，server 内部通过 publisher
//! 推回 `qr_login.scanned` / `qr_login.authorized` / `qr_login.rejected`；TTL 到期
//! server 自己推 `qr_login.expired`（spec §3.3）。

use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::service::qr_login_service::WebDeviceSnapshot;
use msgtrans::SessionId;
use privchat_protocol::rpc::qr_login::{QrLoginCreateSceneRequest, QrLoginCreateSceneResponse};
use serde_json::Value;

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let request: QrLoginCreateSceneRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    if request.web_device_id.trim().is_empty() {
        return Err(RpcError::validation("web_device_id 不能为空".to_string()));
    }

    let session_id = parse_session_id(ctx.session_id.as_deref()).ok_or_else(|| {
        RpcError::internal(
            "qr_login.create_scene: missing or invalid session_id in RpcContext".to_string(),
        )
    })?;

    let snapshot = WebDeviceSnapshot {
        device_name: request
            .web_device_info
            .as_ref()
            .map(|info| info.device_name.clone()),
        user_agent: request
            .web_device_info
            .as_ref()
            .and_then(|info| info.os_version.clone()),
        ip_address: None,
    };

    let scene = services
        .qr_login_service
        .create_scene(
            request.purpose,
            request.web_device_id,
            snapshot,
            request.ttl_secs,
        )
        .await;

    services
        .qr_login_publisher
        .bind(scene.scene_id.clone(), session_id);

    tracing::info!(
        "🪪 qr_login.create_scene: scene_id={} session={}",
        scene.scene_id,
        session_id
    );

    let response = QrLoginCreateSceneResponse {
        rpc_topic: scene.rpc_topic(),
        scene_id: scene.scene_id,
        qr_token: scene.qr_token,
        expires_at: scene.expires_at,
    };

    serde_json::to_value(&response)
        .map_err(|e| RpcError::internal(format!("序列化响应失败: {}", e)))
}

/// `RpcContext.session_id` 形如 `"session-42"`（见 `msgtrans::SessionId::Display`），
/// 反解出原始 `u64`。
fn parse_session_id(s: Option<&str>) -> Option<SessionId> {
    let raw = s?;
    let n: u64 = raw.strip_prefix("session-").unwrap_or(raw).parse().ok()?;
    Some(SessionId::new(n))
}
