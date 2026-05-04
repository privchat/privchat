// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! Unified token 编排服务（spec TOKEN_UNIFICATION_SPEC v1.3 Phase A）。
//!
//! 串起：
//! - [`crate::auth::TokenService`]：RS256 签发 / 验签
//! - [`crate::auth::DeviceManagerDb`]：设备 upsert + `session_version` 真值
//! - [`crate::repository::RefreshTokenRepository`]：refresh token 落库
//!
//! Phase A 行为约束：
//! - **issue 必须 service-only**：service-key gate 在 HTTP 层做（`verify_service_key`）；
//!   本 service 不再二次验证 service key（避免双重耦合），但所有调用点都假定 caller 已校验
//! - **refresh 不 rotate**：本轮不发新 refresh，只回新的 access；只更新 `last_used_at`
//! - **introspect 是权威结果，不缓存**：每次都查 DB
//! - **revoke**：支持 by `jti` 或 by `refresh_token` 明文（计算 hash 后查）
//!
//! 不接管 IM RPC 鉴权 / 不切现有登录链路（spec §10 Phase A 是 additive）。

use std::sync::Arc;

use chrono::Utc;
use uuid::Uuid;

use crate::auth::token_service::{
    IssueClaims, TokenService, VerifyError, TOKEN_TYPE_ACCESS, TOKEN_TYPE_REFRESH,
};
use crate::auth::{Device, DeviceInfo, DeviceManagerDb, DeviceType, SessionState, SessionVerifyResult};
use crate::error::{Result, ServerError};
use crate::repository::{hash_refresh_token, RefreshTokenRepository};

/// `/auth/issue` 业务参数。HTTP 层做 service-key 校验后构造此结构调用。
#[derive(Debug, Clone)]
pub struct IssueParams {
    pub user_id: u64,
    /// 客户端持有 device_id 时传入；否则 server 生成 UUID
    pub device_id: Option<String>,
    pub device_info: DeviceInfo,
    /// 业务模块决定授予的 scope；缺省 `["user"]`
    pub scope: Option<Vec<String>>,
    /// 缺省走 `RsaJwtConfig::default_audience`
    pub audience: Option<Vec<String>>,
    /// 缺省 `"privchat-application"`
    pub business_system_id: Option<String>,
    /// access TTL 覆盖（秒）；None 走 `RsaJwtConfig::access_ttl_secs`
    pub access_ttl_secs: Option<i64>,
    /// 调用方 IP（用于 device.ip_address 字段）
    pub ip_address: String,
}

/// `/auth/issue` 返回（spec §8 LoginResponse 的 server 直出形态）。
#[derive(Debug, Clone)]
pub struct IssueResult {
    pub user_id: u64,
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str, // "Bearer"
    pub expires_in: i64,
    pub refresh_expires_in: i64,
    pub device_id: String,
    pub session_version: i64,
    pub device_created: bool,
    pub scope: Vec<String>,
    pub issuer: String,
    pub audience: Vec<String>,
    pub kid: String,
}

/// `/auth/introspect` 返回。`active=false` 时附带 `reason`。
#[derive(Debug, Clone)]
pub struct IntrospectResult {
    pub active: bool,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub session_version: Option<i64>,
    pub scope: Option<Vec<String>>,
    pub audience: Option<Vec<String>>,
    pub expires_at: Option<i64>,
    pub jti: Option<String>,
    pub reason: Option<String>,
}

impl IntrospectResult {
    fn inactive(reason: &str) -> Self {
        Self {
            active: false,
            user_id: None,
            device_id: None,
            session_version: None,
            scope: None,
            audience: None,
            expires_at: None,
            jti: None,
            reason: Some(reason.to_string()),
        }
    }
}

/// `/auth/revoke` 入参（HTTP 层做参数互斥校验）。
#[derive(Debug, Clone)]
pub enum RevokeRequest {
    ByJti { jti: String },
    ByRefreshToken { refresh_token: String },
}

#[derive(Clone)]
pub struct UnifiedTokenService {
    rsa: Arc<TokenService>,
    devices: Arc<DeviceManagerDb>,
    refresh_repo: Arc<RefreshTokenRepository>,
    /// 默认 audience；issue 未传 audience 时 fallback
    default_audience: Vec<String>,
    /// 默认 issuer（与 RsaJwtConfig.issuer 一致，仅冗余在 IssueResult.issuer 字段输出用）
    issuer: String,
    access_ttl_secs: i64,
    refresh_ttl_secs: i64,
}

impl UnifiedTokenService {
    pub fn new(
        rsa: Arc<TokenService>,
        devices: Arc<DeviceManagerDb>,
        refresh_repo: Arc<RefreshTokenRepository>,
        default_audience: Vec<String>,
        issuer: String,
        access_ttl_secs: i64,
        refresh_ttl_secs: i64,
    ) -> Self {
        Self {
            rsa,
            devices,
            refresh_repo,
            default_audience,
            issuer,
            access_ttl_secs,
            refresh_ttl_secs,
        }
    }

    /// 暴露底层 [`TokenService`]：HTTP `/auth/jwks` 端点直接拿它返 JWKS。
    pub fn token_service(&self) -> &Arc<TokenService> {
        &self.rsa
    }

    /// `/api/service/auth/issue`。spec §6.1 service-only。
    pub async fn issue(&self, params: IssueParams) -> Result<IssueResult> {
        if params.user_id == 0 {
            return Err(ServerError::Validation("user_id 不能为 0".to_string()));
        }

        // 1) 决定 device_id：客户端传入时校验 UUID 格式（与 v1.2 TokenIssueService 行为一致）
        let device_id = match &params.device_id {
            Some(d) if !d.trim().is_empty() => {
                Uuid::parse_str(d).map_err(|_| {
                    ServerError::Validation("device_id 必须是有效的 UUID 格式".to_string())
                })?;
                d.clone()
            }
            _ => Uuid::new_v4().to_string(),
        };

        // 2) UPSERT 设备拿真值 session_version（**不**孤儿 token：device 必先入表）
        let business_system_id = params
            .business_system_id
            .clone()
            .unwrap_or_else(|| "privchat-application".to_string());

        let device = Device {
            device_id: device_id.clone(),
            user_id: params.user_id,
            business_system_id: business_system_id.clone(),
            device_info: params.device_info.clone(),
            device_type: DeviceType::from_app_id(&params.device_info.app_id),
            token_jti: String::new(), // unified token 不复用 device 表的 jti 字段
            session_version: 1,
            session_state: SessionState::Active,
            kicked_at: None,
            kicked_reason: None,
            last_active_at: Utc::now(),
            created_at: Utc::now(),
            ip_address: params.ip_address.clone(),
        };
        let (device_created, session_version) =
            self.devices.register_or_update_device(&device).await?;

        // 3) 决定 scope / audience（spec §6.1.1：trusted application 透传，server 不二次审）
        let scope = params.scope.clone().unwrap_or_else(|| vec!["user".to_string()]);
        let audience = params
            .audience
            .clone()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| self.default_audience.clone());

        // 4) 构造 issue claims
        let issue_claims = IssueClaims {
            user_id: params.user_id,
            device_id: device_id.clone(),
            session_version,
            scope: scope.clone(),
            audience: audience.clone(),
            business_system_id,
            app_id: params.device_info.app_id.clone(),
        };

        // 5) 签 access + refresh
        // Phase A：access TTL 走 config 默认；params.access_ttl_secs 仅日后扩展用，这里保留接口不消费
        let access = self.rsa.issue_access(issue_claims.clone()).map_err(|e| {
            ServerError::Internal(format!("TokenService access 签发失败: {}", e))
        })?;
        let refresh = self.rsa.issue_refresh(issue_claims).map_err(|e| {
            ServerError::Internal(format!("TokenService refresh 签发失败: {}", e))
        })?;

        // 6) refresh 落库（永不存明文，spec §11.2）
        let now_ms = Utc::now().timestamp_millis();
        // refresh.expires_at 是 Unix 秒，转 ms
        let expires_at_ms = refresh.expires_at.saturating_mul(1000);
        self.refresh_repo
            .insert(
                &refresh.jti,
                params.user_id,
                &device_id,
                &refresh.token,
                session_version,
                expires_at_ms,
                now_ms,
            )
            .await
            .map_err(|e| {
                ServerError::Internal(format!("插入 privchat_refresh_tokens 失败: {}", e))
            })?;

        Ok(IssueResult {
            user_id: params.user_id,
            access_token: access.token,
            refresh_token: refresh.token,
            token_type: "Bearer",
            expires_in: self.access_ttl_secs,
            refresh_expires_in: self.refresh_ttl_secs,
            device_id,
            session_version,
            device_created,
            scope,
            issuer: self.issuer.clone(),
            audience,
            kid: self.rsa.kid().to_string(),
        })
    }

    /// `/api/service/auth/refresh`。Phase A **不** rotate refresh，只回新 access + 更新 `last_used_at`。
    ///
    /// 完整校验：签名 / typ=refresh / jti 存在 / token_hash 匹配 / 未过期 / 未 revoked /
    /// device_id 一致 / session_version 仍是 DB 当前值。
    pub async fn refresh(&self, refresh_token: &str, device_id: &str) -> Result<IssueResult> {
        // 1) RS256 验签 + typ=refresh
        let claims = self
            .rsa
            .verify(refresh_token, TOKEN_TYPE_REFRESH)
            .map_err(verify_to_error)?;

        // 2) device_id 一致（防止换设备复用 refresh）
        if claims.device_id != device_id {
            return Err(ServerError::Unauthorized(
                "device_id 与 refresh token 不一致".to_string(),
            ));
        }

        // 3) 查 refresh 记录
        let record = self
            .refresh_repo
            .find_by_jti(&claims.jti)
            .await
            .map_err(|e| ServerError::Internal(format!("查询 refresh token 失败: {}", e)))?
            .ok_or_else(|| ServerError::Unauthorized("refresh token 已撤销或不存在".to_string()))?;

        // 4) token_hash 匹配（防止 jti 已知但拿伪造的明文）
        let provided_hash = hash_refresh_token(refresh_token);
        if !ct_eq(provided_hash.as_bytes(), record.token_hash.as_bytes()) {
            return Err(ServerError::Unauthorized(
                "refresh token 校验失败".to_string(),
            ));
        }

        // 5) revoked / expired
        if record.is_revoked() {
            return Err(ServerError::Unauthorized(
                "refresh token 已撤销".to_string(),
            ));
        }
        let now_ms = Utc::now().timestamp_millis();
        if record.is_expired(now_ms) {
            return Err(ServerError::Unauthorized(
                "refresh token 已过期".to_string(),
            ));
        }

        // 6) device + session_version 真值
        let user_id = claims.sub.parse::<u64>().map_err(|_| {
            ServerError::Internal(format!("refresh token sub 不是有效 u64: {}", claims.sub))
        })?;
        let session_check = self
            .devices
            .verify_device_session(user_id, &claims.device_id, claims.session_version)
            .await?;
        match session_check {
            SessionVerifyResult::Valid {
                session_version: current_version,
            } => {
                // 7) 用真值 session_version 重新签 access（保持与 refresh claim 一致）
                let issue_claims = IssueClaims {
                    user_id,
                    device_id: claims.device_id.clone(),
                    session_version: current_version,
                    scope: claims.scope.clone(),
                    audience: claims.aud.clone(),
                    business_system_id: claims.business_system_id.clone(),
                    app_id: claims.app_id.clone(),
                };
                let access = self.rsa.issue_access(issue_claims).map_err(|e| {
                    ServerError::Internal(format!("TokenService access 签发失败: {}", e))
                })?;

                // 8) Phase A：refresh 不 rotate；touch last_used_at
                self.refresh_repo
                    .touch_last_used(&claims.jti, now_ms)
                    .await
                    .map_err(|e| {
                        ServerError::Internal(format!(
                            "更新 last_used_at 失败: {}",
                            e
                        ))
                    })?;

                Ok(IssueResult {
                    user_id,
                    access_token: access.token,
                    refresh_token: refresh_token.to_string(), // 不 rotate，原样返
                    token_type: "Bearer",
                    expires_in: self.access_ttl_secs,
                    refresh_expires_in: ((record.expires_at - now_ms).max(0)) / 1000,
                    device_id: claims.device_id,
                    session_version: current_version,
                    device_created: false,
                    scope: claims.scope,
                    issuer: self.issuer.clone(),
                    audience: claims.aud,
                    kid: self.rsa.kid().to_string(),
                })
            }
            SessionVerifyResult::DeviceNotFound => Err(ServerError::Unauthorized(
                "设备不存在".to_string(),
            )),
            SessionVerifyResult::SessionInactive { message, .. } => {
                Err(ServerError::Unauthorized(format!(
                    "设备会话已失效: {}",
                    message
                )))
            }
            SessionVerifyResult::VersionMismatch { .. } => Err(ServerError::Unauthorized(
                "session_version 已 bump，refresh 已失效".to_string(),
            )),
        }
    }

    /// `/api/service/auth/introspect`。返回权威 active=true/false（不缓存）。
    pub async fn introspect(&self, token: &str) -> IntrospectResult {
        // 1) 验签：access 或 refresh 都接受（spec 没规定 introspect 限定 access）
        let claims = match self.rsa.verify(token, TOKEN_TYPE_ACCESS) {
            Ok(c) => c,
            Err(VerifyError::Expired) => return IntrospectResult::inactive("expired"),
            Err(VerifyError::UnknownKid) => return IntrospectResult::inactive("unknown_kid"),
            Err(VerifyError::Invalid) => {
                // 也可能是 refresh token；试一次
                match self.rsa.verify(token, TOKEN_TYPE_REFRESH) {
                    Ok(c) => c,
                    Err(VerifyError::Expired) => return IntrospectResult::inactive("expired"),
                    Err(VerifyError::UnknownKid) => {
                        return IntrospectResult::inactive("unknown_kid")
                    }
                    Err(VerifyError::Invalid) => {
                        return IntrospectResult::inactive("signature_invalid")
                    }
                }
            }
        };

        let user_id = match claims.sub.parse::<u64>() {
            Ok(v) => v,
            Err(_) => return IntrospectResult::inactive("signature_invalid"),
        };

        // 2) device + session_version 真值
        let session_check = match self
            .devices
            .verify_device_session(user_id, &claims.device_id, claims.session_version)
            .await
        {
            Ok(r) => r,
            Err(_) => return IntrospectResult::inactive("not_found"),
        };
        match session_check {
            SessionVerifyResult::Valid { .. } => {}
            SessionVerifyResult::DeviceNotFound => {
                return IntrospectResult::inactive("not_found");
            }
            SessionVerifyResult::SessionInactive { .. } => {
                return IntrospectResult::inactive("revoked");
            }
            SessionVerifyResult::VersionMismatch { .. } => {
                return IntrospectResult::inactive("version_mismatch");
            }
        }

        // 3) 如果是 refresh token，还要查 record；如果是 access token，session_version 已足够
        if claims.token_type == TOKEN_TYPE_REFRESH {
            match self.refresh_repo.find_by_jti(&claims.jti).await {
                Ok(Some(rec)) => {
                    if rec.is_revoked() {
                        return IntrospectResult::inactive("revoked");
                    }
                    let now_ms = Utc::now().timestamp_millis();
                    if rec.is_expired(now_ms) {
                        return IntrospectResult::inactive("expired");
                    }
                }
                Ok(None) => return IntrospectResult::inactive("revoked"),
                Err(_) => return IntrospectResult::inactive("not_found"),
            }
        }

        IntrospectResult {
            active: true,
            user_id: Some(user_id),
            device_id: Some(claims.device_id),
            session_version: Some(claims.session_version),
            scope: Some(claims.scope),
            audience: Some(claims.aud),
            expires_at: Some(claims.exp),
            jti: Some(claims.jti),
            reason: None,
        }
    }

    /// `/api/service/auth/revoke`。返回受影响的 refresh 行数（0 = miss）。
    pub async fn revoke(&self, req: RevokeRequest) -> Result<u64> {
        let now_ms = Utc::now().timestamp_millis();
        match req {
            RevokeRequest::ByJti { jti } => self
                .refresh_repo
                .revoke_by_jti(&jti, "revoked_by_jti", now_ms)
                .await
                .map_err(|e| {
                    ServerError::Internal(format!("revoke by jti 失败: {}", e))
                }),
            RevokeRequest::ByRefreshToken { refresh_token } => {
                let token_hash = hash_refresh_token(&refresh_token);
                self.refresh_repo
                    .revoke_by_token_hash(&token_hash, "revoked_by_token", now_ms)
                    .await
                    .map_err(|e| {
                        ServerError::Internal(format!("revoke by refresh_token 失败: {}", e))
                    })
            }
        }
    }
}

/// 把 TokenService 的 [`VerifyError`] 转 [`ServerError`]，统一交给 HTTP 层映射 envelope code。
fn verify_to_error(e: VerifyError) -> ServerError {
    match e {
        VerifyError::Expired => ServerError::Unauthorized("token 已过期".to_string()),
        VerifyError::UnknownKid => {
            ServerError::Unauthorized("token kid 未知".to_string())
        }
        VerifyError::Invalid => {
            ServerError::Unauthorized("token 验签失败".to_string())
        }
    }
}

/// 常数时间字节比较（防 hash compare timing attack）。
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ct_eq_basic() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(ct_eq(b"", b""));
    }

    #[test]
    fn introspect_inactive_helper_sets_reason() {
        let r = IntrospectResult::inactive("revoked");
        assert!(!r.active);
        assert_eq!(r.reason.as_deref(), Some("revoked"));
        assert!(r.user_id.is_none());
    }
}
