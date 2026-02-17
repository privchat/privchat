use crate::auth::models::ImTokenClaims;
use crate::error::{Result, ServerError};
use chrono::Utc;
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use uuid::Uuid;

/// JWT 签发和验证服务
pub struct JwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    issuer: String,
    audience: String,
    token_ttl: i64,
}

impl JwtService {
    /// 创建 JWT 服务 (HS256 对称加密)
    pub fn new(secret: String, token_ttl: i64) -> Self {
        Self {
            encoding_key: EncodingKey::from_secret(secret.as_bytes()),
            decoding_key: DecodingKey::from_secret(secret.as_bytes()),
            issuer: "privchat-im".to_string(),
            audience: "privchat-client".to_string(),
            token_ttl,
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
        let now = Utc::now().timestamp();
        let ttl = custom_ttl.unwrap_or(self.token_ttl);
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
        };

        let header = Header::new(Algorithm::HS256);
        let token = encode(&header, &claims, &self.encoding_key)
            .map_err(|e| ServerError::Internal(format!("JWT 签发失败: {}", e)))?;

        Ok(token)
    }

    /// 验证 token
    pub fn verify_token(&self, token: &str) -> Result<ImTokenClaims> {
        let mut validation = Validation::new(Algorithm::HS256);
        validation.set_issuer(&[&self.issuer]);
        validation.set_audience(&[&self.audience]);

        let token_data = decode::<ImTokenClaims>(token, &self.decoding_key, &validation)
            .map_err(|_e| ServerError::InvalidToken)?;

        Ok(token_data.claims)
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
            .issue_token("alice", "device-123", "ecommerce", "ios", None)
            .unwrap();

        assert!(!token.is_empty());

        // 验证 token
        let claims = jwt_service.verify_token(&token).unwrap();

        assert_eq!(claims.sub, "alice");
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
            .issue_token("alice", "device-123", "ecommerce", "ios", None)
            .unwrap();

        // 验证 token（device_id 匹配）
        let claims = jwt_service
            .verify_token_with_device(&token, "device-123")
            .unwrap();

        assert_eq!(claims.sub, "alice");
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
