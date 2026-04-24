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

//! `account/auth/refresh` —— Access Token 无感续期（Phase B1，non-rotation）
//!
//! 契约参考：`privchat-docs/spec/03-protocol-sdk/TOKEN_REFRESH_SPEC.md` §5
//!
//! B1 仅实现：校验 refresh token + device_id + session_version，签发新的 access token；
//! refresh token 本身原样保留（响应 `refresh_token = None`，由 SDK 维持旧值）。
//! Rotation / grace / reused 由 Phase B2 处理。

use crate::auth::SessionVerifyResult;
use crate::error::ServerError;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::auth::AuthRefreshRequest;
use privchat_protocol::ErrorCode;
use serde_json::{json, Value};
use uuid::Uuid;

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    _ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 refresh token 请求");

    let request: AuthRefreshRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    if request.refresh_token.is_empty() {
        return Err(RpcError::validation("refresh_token 不能为空".to_string()));
    }
    if Uuid::parse_str(&request.device_id).is_err() {
        return Err(RpcError::validation(
            "device_id 必须是有效的 UUID 格式".to_string(),
        ));
    }

    // 1. 校验 refresh token（签名、typ、过期）
    let claims = match services.jwt_service.verify_refresh_token(&request.refresh_token) {
        Ok(claims) => claims,
        Err(ServerError::TokenExpired) => {
            tracing::info!("❌ refresh token 已过期: device={}", request.device_id);
            return Err(RpcError::from_code(
                ErrorCode::RefreshTokenExpired,
                "Refresh token 已过期，请重新登录".to_string(),
            ));
        }
        Err(err) => {
            tracing::warn!(
                "❌ refresh token 校验失败: device={}, err={}",
                request.device_id,
                err
            );
            return Err(RpcError::from_code(
                ErrorCode::InvalidToken,
                "Refresh token 无效".to_string(),
            ));
        }
    };

    // 2. device_id 必须和 token 绑定的一致（防跨设备滥用 → 10010）
    if claims.device_id != request.device_id {
        tracing::warn!(
            "❌ refresh token device 绑定不匹配: token_device={}, request_device={}",
            claims.device_id,
            request.device_id
        );
        return Err(RpcError::from_code(
            ErrorCode::RefreshTokenRevoked,
            "Refresh token 与当前设备不匹配".to_string(),
        ));
    }

    let user_id: u64 = claims.sub.parse().map_err(|_| {
        RpcError::from_code(
            ErrorCode::InvalidToken,
            format!("Refresh token 中 user_id 非法: {}", claims.sub),
        )
    })?;

    // 3. 校验 device 会话状态 + session_version：任何变更均视为 refresh 被撤销（→ 10010）
    let device_id = request.device_id.clone();
    let session_version = claims.session_version;
    match services
        .device_manager_db
        .verify_device_session(user_id, &device_id, session_version)
        .await
        .map_err(|e| RpcError::internal(format!("设备会话校验失败: {}", e)))?
    {
        SessionVerifyResult::Valid { .. } => {}
        SessionVerifyResult::DeviceNotFound => {
            tracing::warn!(
                "❌ refresh 时设备不存在: user={}, device={}",
                user_id,
                device_id
            );
            return Err(RpcError::from_code(
                ErrorCode::RefreshTokenRevoked,
                "设备不存在或已移除，请重新登录".to_string(),
            ));
        }
        SessionVerifyResult::SessionInactive { state, message } => {
            tracing::info!(
                "❌ refresh 时会话状态非 Active: user={}, device={}, state={:?}",
                user_id,
                device_id,
                state
            );
            return Err(RpcError::from_code(
                ErrorCode::RefreshTokenRevoked,
                message,
            ));
        }
        SessionVerifyResult::VersionMismatch {
            token_version,
            current_version,
        } => {
            tracing::info!(
                "❌ refresh 时 session_version 不匹配: user={}, device={}, token_v={}, current_v={}",
                user_id,
                device_id,
                token_version,
                current_version
            );
            return Err(RpcError::from_code(
                ErrorCode::RefreshTokenRevoked,
                "Refresh token 已被撤销（会话版本升级），请重新登录".to_string(),
            ));
        }
    }

    // 4. 签发新的 access token（保持 session_version / business_system_id / app_id）
    let access_token = services
        .jwt_service
        .issue_token_with_version(
            user_id,
            &device_id,
            &claims.business_system_id,
            &claims.app_id,
            session_version,
            None,
        )
        .map_err(|e| RpcError::internal(format!("签发 access token 失败: {}", e)))?;

    let token_ttl = services.jwt_service.default_ttl();
    let expires_at =
        (chrono::Utc::now() + chrono::Duration::seconds(token_ttl)).timestamp_millis();
    let expires_at_u64 = expires_at.max(0) as u64;

    tracing::debug!(
        "✅ access token 已续期: user={}, device={}, session_version={}, expires_at={}",
        user_id,
        device_id,
        session_version,
        expires_at
    );

    // 5. 返回新 access token；B1 非 rotation，refresh_token 省略，SDK 保留旧值
    Ok(json!({
        "access_token": access_token,
        "expires_at": expires_at_u64,
    }))
}
