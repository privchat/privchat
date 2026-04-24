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

use crate::auth::models::ImTokenClaims;
use crate::error::{Result, ServerError};
use chrono::Utc;
use jsonwebtoken::{decode, encode, errors::ErrorKind, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use uuid::Uuid;

/// JWT 签发和验证服务
pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    issuer: String,
    audience: String,
    token_ttl: i64,
    refresh_token_ttl: i64,
}

/// Access token 的 `typ` 值。
pub const TOKEN_TYPE_ACCESS: &str = "access";
/// Refresh token 的 `typ` 值。
pub const TOKEN_TYPE_REFRESH: &str = "refresh";

impl JwtService {
    /// 创建 JWT 服务 (HS256 对称加密)。`refresh_token_ttl` 默认取 `token_ttl * 4`。
    pub fn new(secret: String, token_ttl: i64) -> Self {
        Self::new_with_ttls(secret, token_ttl, token_ttl.saturating_mul(4))
    }

    /// 创建 JWT 服务并显式指定 access / refresh 的 TTL（秒）。
    pub fn new_with_ttls(secret: String, token_ttl: i64, refresh_token_ttl: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            issuer: "privchat-im".to_string(),
            audience: "privchat-client".to_string(),
            token_ttl,
            refresh_token_ttl,
        }
    }

    /// 签发 token（默认使用 session_version = 1）
    pub fn issue_token(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        custom_ttl: Option<i64>,
    ) -> Result<String> {
        self.issue_token_with_version(
            user_id,
            device_id,
            business_system_id,
            app_id,
            1, // 默认版本号
            custom_ttl,
        )
    }

    /// 签发 token（指定 session_version）
    pub fn issue_token_with_version(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
        custom_ttl: Option<i64>,
    ) -> Result<String> {
        self.issue_token_inner(
            user_id,
            device_id,
            business_system_id,
            app_id,
            session_version,
            custom_ttl.unwrap_or(self.token_ttl),
            TOKEN_TYPE_ACCESS,
        )
    }

    /// 签发 refresh token（Phase B1：不做 rotation，直接长 TTL）。
    pub fn issue_refresh_token(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
    ) -> Result<String> {
        self.issue_token_inner(
            user_id,
            device_id,
            business_system_id,
            app_id,
            session_version,
            self.refresh_token_ttl,
            TOKEN_TYPE_REFRESH,
        )
    }

    fn issue_token_inner(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
        ttl: i64,
        typ: &str,
    ) -> Result<String> {
        let now = Utc::now().timestamp();
        let jti = Uuid::new_v4().to_string();

        let claims = ImTokenClaims {
            iss: self.issuer.clone(),
            sub: user_id.to_string(),
            aud: self.audience.clone(),
            exp: now + ttl,
            iat: now,
            jti,
            device_id: device_id.to_string(),
            business_system_id: business_system_id.to_string(),
            app_id: app_id.to_string(),
            session_version,
            typ: typ.to_string(),
        };

        let header = Header::new(Algorithm::HS256);
        let token = encode(&header, &claims, &self.encoding_key)
            .map_err(|e| ServerError::Internal(format!("JWT 签发失败: {}", e)))?;

        Ok(token)
    }

    /// 解码 token（不做 typ 校验，内部共用）。
    fn decode_token(&self, token: &str) -> Result<ImTokenClaims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);

        let token_data = decode::<ImTokenClaims>(token, &self.decoding_key, &validation)
            .map_err(|e| match e.kind() {
                ErrorKind::ExpiredSignature => ServerError::TokenExpired,
                _ => ServerError::InvalidToken,
            })?;

        Ok(token_data.claims)
    }

    /// 验证 access token。refresh token 会被拒绝为 `InvalidToken`，防止跨用途。
    pub fn verify_token(&self, token: &str) -> Result<ImTokenClaims> {
        let claims = self.decode_token(token)?;
        if claims.typ == TOKEN_TYPE_REFRESH {
            tracing::warn!(
                "❌ refresh token 被用于 access 路径: jti={}, user={}, device={}",
                claims.jti,
                claims.sub,
                claims.device_id
            );
            return Err(ServerError::InvalidToken);
        }
        Ok(claims)
    }

    /// 验证 refresh token。仅接受 `typ == "refresh"`。
    pub fn verify_refresh_token(&self, token: &str) -> Result<ImTokenClaims> {
        let claims = self.decode_token(token)?;
        if claims.typ != TOKEN_TYPE_REFRESH {
            tracing::warn!(
                "❌ access token 被用于 refresh 路径: jti={}, user={}, device={}, typ={}",
                claims.jti,
                claims.sub,
                claims.device_id,
                claims.typ
            );
            return Err(ServerError::InvalidToken);
        }
        Ok(claims)
    }

    /// 验证 token 并检查 device_id 匹配
    ///
    /// 安全说明：
    /// - 验证 JWT 签名和过期时间
    /// - 验证 token 中的 device_id 是否与请求中的 device_id 匹配
    /// - 防止 token 被复制到其他设备使用
    pub fn verify_token_with_device(
        &self,
        token: &str,
        request_device_id: &str,
    ) -> Result<ImTokenClaims> {
        // 1. 验证 token 基本信息
        let claims = self.verify_token(token)?;

        // 2. 验证 device_id 是否匹配
        if claims.device_id != request_device_id {
            tracing::warn!(
                "设备ID不匹配: token_device_id={}, request_device_id={}",
                claims.device_id,
                request_device_id
            );
            return Err(ServerError::Forbidden(
                "Token 绑定的设备与请求设备不匹配，请重新登录".to_string(),
            ));
        }

        Ok(claims)
    }

    /// 获取默认 TTL
    pub fn default_ttl(&self) -> i64 {
        self.token_ttl
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwt_issue_and_verify() {
        let jwt_service = JwtService::new("test-secret-key-at-least-32-chars".to_string(), 3600);

        // 签发 token
        let token = jwt_service
            .issue_token(1001, "device-123", "ecommerce", "ios", None)
            .unwrap();

        assert!(!token.is_empty());

        // 验证 token
        let claims = jwt_service.verify_token(&token).unwrap();

        assert_eq!(claims.sub, "1001");
        assert_eq!(claims.device_id, "device-123");
        assert_eq!(claims.business_system_id, "ecommerce");
        assert_eq!(claims.app_id, "ios");
    }

    #[test]
    fn test_jwt_verify_invalid_token() {
        let jwt_service = JwtService::new("test-secret-key-at-least-32-chars".to_string(), 3600);

        // 验证无效 token
        let result = jwt_service.verify_token("invalid.token.here");
        assert!(result.is_err());
    }

    #[test]
    fn test_jwt_verify_with_device_id() {
        let jwt_service = JwtService::new("test-secret-key-at-least-32-chars".to_string(), 3600);

        // 签发 token
        let token = jwt_service
            .issue_token(1001, "device-123", "ecommerce", "ios", None)
            .unwrap();

        // 验证 token（device_id 匹配）
        let claims = jwt_service
            .verify_token_with_device(&token, "device-123")
            .unwrap();

        assert_eq!(claims.sub, "1001");
        assert_eq!(claims.device_id, "device-123");

        // 验证 token（device_id 不匹配）
        let result = jwt_service.verify_token_with_device(&token, "device-456");

        assert!(result.is_err());
        match result {
            Err(ServerError::Forbidden(msg)) => {
                assert!(msg.contains("设备不匹配"));
            }
            _ => panic!("Expected Forbidden error"),
        }
    }
}
