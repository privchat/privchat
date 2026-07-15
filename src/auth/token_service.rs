// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! 统一 token 服务（spec TOKEN_UNIFICATION_SPEC v1.3 §4 / §6）。
//!
//! HTTP API（/api/service/auth/issue 等）与 IM RPC（AuthorizationRequest）使用
//! **同一** [`TokenService`] 实例签发与验证。算法由 `[auth.jwt] algorithm` 决定：
//! - [`JwtAlgorithm::Hs256`]：对称 secret，简单部署
//! - [`JwtAlgorithm::Rs256`]：非对称 PEM key + JWKS 暴露公钥
//!
//! Claim 形态见 spec §4：固定 `aud=Vec<String>`、`session_version`、`scope`、`kid`（仅 RS256 header）。

use std::fs;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::Utc;
use jsonwebtoken::{
    decode, decode_header, encode, errors::ErrorKind as JwtErrorKind, Algorithm, DecodingKey,
    EncodingKey, Header, Validation,
};
use rsa::pkcs1::DecodeRsaPublicKey;
use rsa::pkcs8::DecodePublicKey;
use rsa::traits::PublicKeyParts;
use rsa::RsaPublicKey;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::{JwtAlgorithm, JwtConfig};
use crate::error::{Result, ServerError};

/// `typ` 字段：Access token。
pub const TOKEN_TYPE_ACCESS: &str = "access";
/// `typ` 字段：Refresh token。
pub const TOKEN_TYPE_REFRESH: &str = "refresh";

/// 统一 token claims（spec §4）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedTokenClaims {
    pub iss: String,
    pub sub: String,
    pub aud: Vec<String>,
    pub iat: i64,
    pub exp: i64,
    pub jti: String,
    #[serde(rename = "typ")]
    pub token_type: String,
    pub device_id: String,
    pub session_version: i64,
    pub scope: Vec<String>,
    pub business_system_id: String,
    pub app_id: String,
}

/// 签发参数。
#[derive(Debug, Clone)]
pub struct IssueClaims {
    pub user_id: u64,
    pub device_id: String,
    pub session_version: i64,
    pub scope: Vec<String>,
    pub audience: Vec<String>,
    pub business_system_id: String,
    pub app_id: String,
}

/// 签发结果（含原始 token 文本 + 关键 claim 摘要）。
#[derive(Debug, Clone)]
pub struct IssuedToken {
    pub token: String,
    pub jti: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub claims: UnifiedTokenClaims,
}

/// JWKS 条目（仅 RS256 模式下暴露）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkRsa {
    pub kid: String,
    pub kty: String,
    pub alg: String,
    #[serde(rename = "use")]
    pub use_: String,
    /// RSA 模数 (base64url-no-pad)
    pub n: String,
    /// RSA 公钥指数 (base64url-no-pad)
    pub e: String,
}

/// JWKS 响应（HS256 模式下 keys 为空）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<JwkRsa>,
}

/// 统一 token 服务。HTTP + IM 共用同一实例。
pub struct TokenService {
    algorithm: Algorithm,
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// JWKS 暴露用的当前公钥；仅 RS256 模式下有值
    jwk: Option<JwkRsa>,
    config: JwtConfig,
}

impl std::fmt::Debug for TokenService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenService")
            .field("algorithm", &self.config.algorithm)
            .field("kid", &self.config.kid)
            .field("issuer", &self.config.issuer)
            .field("default_audience", &self.config.default_audience)
            .field("access_ttl_secs", &self.config.access_ttl_secs)
            .field("refresh_ttl_secs", &self.config.refresh_ttl_secs)
            .finish()
    }
}

impl TokenService {
    /// 从 [`JwtConfig`] 加载密钥（按算法分支）。
    pub fn from_config(config: JwtConfig) -> Result<Self> {
        config
            .validate()
            .map_err(|e| ServerError::Internal(format!("TokenService: 配置校验失败: {}", e)))?;

        match config.algorithm {
            JwtAlgorithm::Hs256 => {
                let secret = config.secret.as_bytes();
                let encoding_key = EncodingKey::from_secret(secret);
                let decoding_key = DecodingKey::from_secret(secret);
                Ok(Self {
                    algorithm: Algorithm::HS256,
                    encoding_key,
                    decoding_key,
                    jwk: None,
                    config,
                })
            }
            JwtAlgorithm::Rs256 => {
                let priv_pem = fs::read(&config.private_key_path).map_err(|e| {
                    ServerError::Internal(format!(
                        "TokenService: 读私钥失败 path={} err={}",
                        config.private_key_path, e
                    ))
                })?;
                let pub_pem = fs::read(&config.public_key_path).map_err(|e| {
                    ServerError::Internal(format!(
                        "TokenService: 读公钥失败 path={} err={}",
                        config.public_key_path, e
                    ))
                })?;

                let encoding_key = EncodingKey::from_rsa_pem(&priv_pem).map_err(|e| {
                    ServerError::Internal(format!("TokenService: 解析私钥 PEM 失败: {}", e))
                })?;
                let decoding_key = DecodingKey::from_rsa_pem(&pub_pem).map_err(|e| {
                    ServerError::Internal(format!("TokenService: 解析公钥 PEM 失败: {}", e))
                })?;

                let jwk = build_jwk_from_pem(&pub_pem, &config.kid)?;

                Ok(Self {
                    algorithm: Algorithm::RS256,
                    encoding_key,
                    decoding_key,
                    jwk: Some(jwk),
                    config,
                })
            }
        }
    }

    /// 算法（HS256 / RS256）。
    pub fn algorithm(&self) -> JwtAlgorithm {
        self.config.algorithm
    }

    /// 当前签名 key 的 kid（HS256 也写 kid 便于轮换审计）。
    pub fn kid(&self) -> &str {
        &self.config.kid
    }

    /// JWKS 端点返回的 key set。HS256 模式下返回空集合（无对外公开的公钥）。
    pub fn jwks(&self) -> JwkSet {
        match &self.jwk {
            Some(k) => JwkSet {
                keys: vec![k.clone()],
            },
            None => JwkSet { keys: Vec::new() },
        }
    }

    /// 颁发 access token。
    pub fn issue_access(&self, params: IssueClaims) -> Result<IssuedToken> {
        self.issue_inner(params, TOKEN_TYPE_ACCESS, self.config.access_ttl_secs)
    }

    /// 颁发 refresh token。
    pub fn issue_refresh(&self, params: IssueClaims) -> Result<IssuedToken> {
        self.issue_inner(params, TOKEN_TYPE_REFRESH, self.config.refresh_ttl_secs)
    }

    fn issue_inner(&self, params: IssueClaims, typ: &str, ttl_secs: i64) -> Result<IssuedToken> {
        let now = Utc::now().timestamp();
        let exp = now + ttl_secs;
        let jti = Uuid::new_v4().to_string();
        let claims = UnifiedTokenClaims {
            iss: self.config.issuer.clone(),
            sub: params.user_id.to_string(),
            aud: params.audience,
            iat: now,
            exp,
            jti: jti.clone(),
            token_type: typ.to_string(),
            device_id: params.device_id,
            session_version: params.session_version,
            scope: params.scope,
            business_system_id: params.business_system_id,
            app_id: params.app_id,
        };

        let mut header = Header::new(self.algorithm);
        header.kid = Some(self.config.kid.clone());

        let token = encode(&header, &claims, &self.encoding_key).map_err(|e| {
            ServerError::Internal(format!("TokenService: encode token 失败: {}", e))
        })?;

        Ok(IssuedToken {
            token,
            jti,
            issued_at: now,
            expires_at: exp,
            claims,
        })
    }

    /// 验签 + 校验 exp / iss / aud / typ。
    ///
    /// **不**在此层做 `session_version` / token revocation 校验（要查 DB / Redis），
    /// 调用方拿 claims 后自行查。
    pub fn verify(
        &self,
        token: &str,
        expected_typ: &str,
    ) -> std::result::Result<UnifiedTokenClaims, VerifyError> {
        let header = decode_header(token).map_err(|_| VerifyError::Invalid)?;
        if header.alg != self.algorithm {
            return Err(VerifyError::Invalid);
        }
        // RS256 模式下校验 kid 一致；HS256 模式下不强制（kid 仅作元信息）
        if matches!(self.config.algorithm, JwtAlgorithm::Rs256) {
            if let Some(kid) = header.kid.as_deref() {
                if kid != self.config.kid {
                    return Err(VerifyError::UnknownKid);
                }
            }
        }

        let mut validation = Validation::new(self.algorithm);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        let aud_refs: Vec<&str> = self
            .config
            .default_audience
            .iter()
            .map(|s| s.as_str())
            .collect();
        validation.set_audience(&aud_refs);
        validation.leeway = 5;

        let data =
            decode::<UnifiedTokenClaims>(token, &self.decoding_key, &validation).map_err(|e| {
                match e.kind() {
                    JwtErrorKind::ExpiredSignature => VerifyError::Expired,
                    JwtErrorKind::InvalidSignature
                    | JwtErrorKind::InvalidToken
                    | JwtErrorKind::Base64(_)
                    | JwtErrorKind::Json(_)
                    | JwtErrorKind::Crypto(_) => VerifyError::Invalid,
                    JwtErrorKind::InvalidAudience | JwtErrorKind::InvalidIssuer => {
                        VerifyError::Invalid
                    }
                    _ => VerifyError::Invalid,
                }
            })?;

        if data.claims.token_type != expected_typ {
            return Err(VerifyError::Invalid);
        }
        Ok(data.claims)
    }

    /// 验签 access token（短路径）。
    pub fn verify_access(
        &self,
        token: &str,
    ) -> std::result::Result<UnifiedTokenClaims, VerifyError> {
        self.verify(token, TOKEN_TYPE_ACCESS)
    }

    /// 验签 refresh token。
    pub fn verify_refresh(
        &self,
        token: &str,
    ) -> std::result::Result<UnifiedTokenClaims, VerifyError> {
        self.verify(token, TOKEN_TYPE_REFRESH)
    }

    // ─────────── 老 IM RPC 调用方便桥（spec §8 unified 形态，签 token 时填默认 audience/scope）───────────

    /// 默认 access token TTL（秒）—— 兼容老调用方 `default_ttl()`。
    pub fn default_ttl(&self) -> i64 {
        self.config.access_ttl_secs
    }

    /// 默认 refresh token TTL（秒）。
    pub fn default_refresh_ttl(&self) -> i64 {
        self.config.refresh_ttl_secs
    }

    /// 默认签发 claim 模板（audience / scope 走 config 默认）。
    fn default_issue_claims(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
    ) -> IssueClaims {
        IssueClaims {
            user_id,
            device_id: device_id.to_string(),
            session_version,
            scope: vec!["user".to_string()],
            audience: self.config.default_audience.clone(),
            business_system_id: business_system_id.to_string(),
            app_id: app_id.to_string(),
        }
    }

    /// 兼容老调用：签发 access token 字符串（带 session_version）。
    pub fn issue_token_with_version(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
        _ttl_override_secs: Option<i64>,
    ) -> Result<String> {
        let claims = self.default_issue_claims(
            user_id,
            device_id,
            business_system_id,
            app_id,
            session_version,
        );
        self.issue_access(claims).map(|t| t.token)
    }

    /// 兼容老调用：签发 refresh token 字符串。
    pub fn issue_refresh_token(
        &self,
        user_id: u64,
        device_id: &str,
        business_system_id: &str,
        app_id: &str,
        session_version: i64,
    ) -> Result<String> {
        let claims = self.default_issue_claims(
            user_id,
            device_id,
            business_system_id,
            app_id,
            session_version,
        );
        self.issue_refresh(claims).map(|t| t.token)
    }

    /// 兼容老调用：验 access token，返回 [`UnifiedTokenClaims`]。
    /// 错误映射到 [`ServerError`]（[`VerifyError::Expired`] → `TokenExpired`）。
    pub fn verify_token(&self, token: &str) -> Result<UnifiedTokenClaims> {
        self.verify_access(token).map_err(|e| match e {
            VerifyError::Expired => ServerError::TokenExpired,
            _ => ServerError::InvalidToken,
        })
    }

    /// 兼容老调用：验 refresh token。
    pub fn verify_refresh_token(&self, token: &str) -> Result<UnifiedTokenClaims> {
        self.verify_refresh(token).map_err(|e| match e {
            VerifyError::Expired => ServerError::TokenExpired,
            _ => ServerError::InvalidToken,
        })
    }

    /// 兼容老调用：验 access token + 校验 device_id 与 token claim 一致。
    pub fn verify_token_with_device(
        &self,
        token: &str,
        request_device_id: &str,
    ) -> Result<UnifiedTokenClaims> {
        let claims = self.verify_token(token)?;
        if claims.device_id != request_device_id {
            return Err(ServerError::InvalidToken);
        }
        Ok(claims)
    }
}

/// 验签错误码（与 spec §11.6 / §6.1 introspect 的 `reason` 字段对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// 签名 / 格式 / typ 错误
    Invalid,
    /// `exp < now`
    Expired,
    /// `kid` 与当前签名 key 不匹配（仅 RS256 模式有意义）
    UnknownKid,
}

impl VerifyError {
    /// introspect 响应里的 `reason` 字符串。
    pub fn as_reason(&self) -> &'static str {
        match self {
            Self::Invalid => "signature_invalid",
            Self::Expired => "expired",
            Self::UnknownKid => "unknown_kid",
        }
    }
}

/// 把 PEM 公钥解出来转成 JWKS 字段（n + e）。支持 PKCS#1 与 PKCS#8 PEM。
fn build_jwk_from_pem(pem: &[u8], kid: &str) -> Result<JwkRsa> {
    let pem_str = std::str::from_utf8(pem)
        .map_err(|e| ServerError::Internal(format!("公钥 PEM 不是 UTF-8: {}", e)))?;

    let public = if pem_str.contains("BEGIN PUBLIC KEY") {
        RsaPublicKey::from_public_key_pem(pem_str)
            .map_err(|e| ServerError::Internal(format!("PKCS#8 公钥解析失败: {}", e)))?
    } else {
        RsaPublicKey::from_pkcs1_pem(pem_str)
            .map_err(|e| ServerError::Internal(format!("PKCS#1 公钥解析失败: {}", e)))?
    };

    let n_bytes = public.n().to_bytes_be();
    let e_bytes = public.e().to_bytes_be();
    Ok(JwkRsa {
        kid: kid.to_string(),
        kty: "RSA".to_string(),
        alg: "RS256".to_string(),
        use_: "sig".to_string(),
        n: URL_SAFE_NO_PAD.encode(n_bytes),
        e: URL_SAFE_NO_PAD.encode(e_bytes),
    })
}
