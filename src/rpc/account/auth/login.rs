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

use crate::auth::verify_password;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::auth::AuthLoginRequest;
use serde_json::{json, Value};
use uuid::Uuid;

/// 处理 用户登录 请求
///
/// 根据服务器配置 `use_internal_auth` 决定是否启用：
/// - true: 使用服务器内置的登录功能，返回 JWT token（适合独立部署）
/// - false: 返回错误，提示使用外部认证系统（适合企业集成）
///
/// 登录成功后返回 JWT token，客户端可直接用于 AuthorizationRequest
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    _ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理用户登录请求: {:?}", body);

    // 检查是否启用内置账号系统
    if !services.config.use_internal_auth {
        tracing::warn!("❌ 内置账号系统已禁用，请使用外部认证系统");
        return Err(RpcError::forbidden(
            "内置账号系统已禁用。请使用外部认证系统获取 token，然后通过 AuthorizationRequest 建立连接。".to_string()
        ));
    }

    // ✨ 使用协议层类型自动反序列化
    let request: AuthLoginRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    let account = request.username.trim().to_lowercase();
    if account.is_empty() {
        return Err(RpcError::validation("用户名不能为空".to_string()));
    }
    let password = &request.password;

    // 1. 从数据库查找用户（支持用户名或邮箱登录，统一按小写比较）
    let user_by_username = services
        .user_repository
        .find_by_username(&account)
        .await
        .map_err(|e| RpcError::internal(format!("查询用户失败: {}", e)))?;
    let user = match user_by_username {
        Some(user) => user,
        None => services
            .user_repository
            .find_by_email(&account)
            .await
            .map_err(|e| RpcError::internal(format!("查询用户失败: {}", e)))?
            .ok_or_else(|| RpcError::unauthorized("账号密码不匹配".to_string()))?,
    };

    // 2. 检查密码哈希是否存在
    let password_hash = user.password_hash.ok_or_else(|| {
        RpcError::unauthorized("此账号未设置密码，请使用外部认证系统登录".to_string())
    })?;

    // 3. 验证密码
    let valid = verify_password(password, &password_hash)
        .map_err(|e| RpcError::internal(format!("密码验证失败: {}", e)))?;

    if !valid {
        tracing::warn!("❌ 密码验证失败: account={}", account);
        return Err(RpcError::unauthorized("账号密码不匹配".to_string()));
    }

    let user_id = user.id;
    let device_id = &request.device_id;

    // 验证 device_id 必须是 UUID 格式（与认证时的验证保持一致）
    if Uuid::parse_str(device_id).is_err() {
        return Err(RpcError::validation(
            "device_id 必须是有效的 UUID 格式".to_string(),
        ));
    }

    // 4. 从 device_info 中提取 app_id 和 device_type
    let (app_id, device_type_str) = request
        .device_info
        .as_ref()
        .map(|info| (info.app_id.clone(), info.device_type.as_str()))
        .unwrap_or_else(|| ("unknown".to_string(), "unknown"));

    // 5. 查询或创建设备，获取 session_version ✨
    let session_version = match services
        .device_manager_db
        .get_device_with_version(user_id, device_id)
        .await
        .map_err(|e| RpcError::internal(format!("查询设备失败: {}", e)))?
    {
        Some((_, version)) => {
            tracing::debug!(
                "✅ 设备已存在: device_id={}, session_version={}",
                device_id,
                version
            );
            version
        }
        None => {
            // 设备不存在，创建新设备
            tracing::debug!("🆕 创建新设备: device_id={}", device_id);
            let device = crate::auth::Device {
                device_id: device_id.to_string(),
                user_id,
                business_system_id: "privchat-internal".to_string(),
                device_info: crate::auth::DeviceInfo {
                    app_id: app_id.clone(),
                    device_name: format!("{} Device", app_id),
                    device_model: "Unknown".to_string(),
                    os_version: "Unknown".to_string(),
                    app_version: "1.0.0".to_string(),
                },
                device_type: match device_type_str {
                    "ios" => crate::auth::DeviceType::IOS,
                    "android" => crate::auth::DeviceType::Android,
                    "web" => crate::auth::DeviceType::Web,
                    "macos" => crate::auth::DeviceType::MacOS,
                    "windows" => crate::auth::DeviceType::Windows,
                    _ => crate::auth::DeviceType::Unknown,
                },
                token_jti: String::new(),
                session_version: 1, // 新设备，版本号为 1
                session_state: crate::auth::SessionState::Active,
                kicked_at: None,
                kicked_reason: None,
                last_active_at: chrono::Utc::now(),
                created_at: chrono::Utc::now(),
                ip_address: "127.0.0.1".to_string(), // TODO: 从请求中获取真实IP
            };

            services
                .device_manager_db
                .register_or_update_device(&device)
                .await
                .map_err(|e| RpcError::internal(format!("注册设备失败: {}", e)))?;

            tracing::debug!(
                "✅ 新设备已注册: device_id={}, session_version=1",
                device_id
            );
            1 // 新设备使用版本号 1
        }
    };

    // 6. 使用 JWT 服务生成真实的 JWT token（带 session_version）✨
    // Token payload 包含：
    // - sub: user_id（用户ID）
    // - device_id: 设备ID（用于验证）
    // - business_system_id: 业务系统ID（内置认证使用 "privchat-internal"）
    // - app_id: 应用ID（从 device_info 获取）
    // - session_version: 会话版本号（用于设备级撤销）✨ 新增
    // - jti: JWT ID（用于撤销）
    // - iss, aud, exp, iat: JWT 标准字段
    let token = services
        .jwt_service
        .issue_token_with_version(
            user_id,
            device_id,
            "privchat-internal", // 内置认证系统的业务系统ID
            &app_id,
            session_version, // ✨ 使用设备的 session_version
            None,            // 使用默认 TTL
        )
        .map_err(|e| RpcError::internal(format!("生成 JWT token 失败: {}", e)))?;

    // 7. 计算过期时间
    let token_ttl = services.jwt_service.default_ttl();
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(token_ttl)).timestamp_millis();

    // 7.1 签发 refresh token（B1 non-rotation，typ=refresh，长 TTL）
    let refresh_token = services
        .jwt_service
        .issue_refresh_token(
            user_id,
            device_id,
            "privchat-internal",
            &app_id,
            session_version,
        )
        .map_err(|e| RpcError::internal(format!("生成 refresh token 失败: {}", e)))?;

    tracing::debug!(
        "✅ 用户登录成功: account={}, user_id={}, device_id={}, device_type={}, app_id={}",
        account,
        user_id,
        device_id,
        device_type_str,
        app_id
    );

    // 8. 返回统一的 AuthResponse 格式（包含 device_id）
    Ok(json!({
        "success": true,
        "user_id": user_id,
        "token": token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "device_id": device_id,
        "message": "登录成功"
    }))
}
