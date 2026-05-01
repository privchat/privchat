// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! RS256 unified token service（spec TOKEN_UNIFICATION_SPEC v1.3 Phase A）。
//!
//! 与现有 [`crate::auth::jwt_service::JwtService`] 是 **sibling** 关系：
//! - `JwtService`（HS256）继续负责 v1.2 IM token 颁发 / 验证，**不动**
//! - `RsaJwtService`（RS256）负责 v1.3 unified token：spec §6 的 issue/refresh/introspect 路径
//!
//! Phase A 的关键约束：
//! - 不接管 IM RPC 的鉴权（spec §10 Phase A 是 additive，不切登录链路）
//! - claim 形态见 spec §4：dual `aud`、`session_version`、`scope`、`kid` 等
//! - 私钥 PEM 路径来自 [`crate::config::RsaJwtConfig`]，文件**不入 repo**（见 `keys/README.md`）

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

use crate::config::RsaJwtConfig;
use crate::error::{Result, ServerError};

/// `typ` 字段：Access token。
pub const TOKEN_TYPE_ACCESS: &str = "access";
/// `typ` 字段：Refresh token。
pub const TOKEN_TYPE_REFRESH: &str = "refresh";

/// Unified token claims（spec §4）。
///
/// 与 v1.2 [`crate::auth::models::ImTokenClaims`] 的差别：
/// - `aud` 是 **数组**（`Vec<String>`），允许同一 token 被 application + server 同时接受
/// - 显式 `scope` claim
/// - 显式 `kid` 由 JWT header 表达（不在 payload）
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

/// 签发结果。
#[derive(Debug, Clone)]
pub struct IssuedToken {
    pub token: String,
    pub jti: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub claims: UnifiedTokenClaims,
}

/// 单条 JWKS key（spec §6.1 `GET /.well-known/jwks.json`）。
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

/// JWKS 响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkSet {
    pub keys: Vec<JwkRsa>,
}

/// RS256 unified token 服务。
///
/// Phase A 单 key（`config.kid` 一份）。多 key + grace window（spec §11.1）留 Phase A 之后扩展。
pub struct RsaJwtService {
    encoding_key: EncodingKey,
    decoding_key: DecodingKey,
    /// JWKS 暴露用的当前公钥
    jwk: JwkRsa,
    config: RsaJwtConfig,
}

impl std::fmt::Debug for RsaJwtService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RsaJwtService")
            .field("kid", &self.jwk.kid)
            .field("issuer", &self.config.issuer)
            .field("default_audience", &self.config.default_audience)
            .field("access_ttl_secs", &self.config.access_ttl_secs)
            .field("refresh_ttl_secs", &self.config.refresh_ttl_secs)
            .finish()
    }
}

impl RsaJwtService {
    /// 从 [`RsaJwtConfig`] 加载 PEM 私钥 + 公钥。
    ///
    /// 调用方**应**在调用前用 [`RsaJwtConfig::is_enabled`] 判断是否启用；
    /// 如果未配置 RS256，构造 [`RsaJwtService`] 没意义（应让 `Option<RsaJwtService>` 留空）。
    pub fn from_config(config: RsaJwtConfig) -> Result<Self> {
        if !config.is_enabled() {
            return Err(ServerError::Internal(
                "RsaJwtService: private/public key path 未配置".to_string(),
            ));
        }

        let priv_pem = fs::read(&config.private_key_path).map_err(|e| {
            ServerError::Internal(format!(
                "RsaJwtService: 读私钥失败 path={} err={}",
                config.private_key_path, e
            ))
        })?;
        let pub_pem = fs::read(&config.public_key_path).map_err(|e| {
            ServerError::Internal(format!(
                "RsaJwtService: 读公钥失败 path={} err={}",
                config.public_key_path, e
            ))
        })?;

        let encoding_key = EncodingKey::from_rsa_pem(&priv_pem).map_err(|e| {
            ServerError::Internal(format!("RsaJwtService: 解析私钥 PEM 失败: {}", e))
        })?;
        let decoding_key = DecodingKey::from_rsa_pem(&pub_pem).map_err(|e| {
            ServerError::Internal(format!("RsaJwtService: 解析公钥 PEM 失败: {}", e))
        })?;

        let jwk = build_jwk_from_pem(&pub_pem, &config.kid)?;

        Ok(Self {
            encoding_key,
            decoding_key,
            jwk,
            config,
        })
    }

    /// JWKS 端点返回的 key set（Phase A 单 key）。
    pub fn jwks(&self) -> JwkSet {
        JwkSet {
            keys: vec![self.jwk.clone()],
        }
    }

    /// 当前签名 key 的 kid。
    pub fn kid(&self) -> &str {
        &self.jwk.kid
    }

    /// 颁发 access token。
    pub fn issue_access(&self, params: IssueClaims) -> Result<IssuedToken> {
        self.issue_inner(params, TOKEN_TYPE_ACCESS, self.config.access_ttl_secs)
    }

    /// 颁发 refresh token。
    pub fn issue_refresh(&self, params: IssueClaims) -> Result<IssuedToken> {
        self.issue_inner(params, TOKEN_TYPE_REFRESH, self.config.refresh_ttl_secs)
    }

    fn issue_inner(
        &self,
        params: IssueClaims,
        typ: &str,
        ttl_secs: i64,
    ) -> Result<IssuedToken> {
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

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.config.kid.clone());

        let token = encode(&header, &claims, &self.encoding_key).map_err(|e| {
            ServerError::Internal(format!("RsaJwtService: encode token 失败: {}", e))
        })?;

        Ok(IssuedToken {
            token,
            jti,
            issued_at: now,
            expires_at: exp,
            claims,
        })
    }

    /// 验签 + 校验 exp / iss / aud / typ。返回 claims；失败返回 [`VerifyError`]。
    ///
    /// **不**在此层做 `session_version` 校验（要查 DB），调用方拿 claims 后自行查。
    pub fn verify(&self, token: &str, expected_typ: &str) -> std::result::Result<UnifiedTokenClaims, VerifyError> {
        // 先解 header 看 kid（Phase A 单 key，仍校验一致）
        let header = decode_header(token).map_err(|_| VerifyError::Invalid)?;
        match header.alg {
            Algorithm::RS256 => {}
            _ => return Err(VerifyError::Invalid),
        }
        if let Some(kid) = header.kid.as_deref() {
            if kid != self.config.kid {
                return Err(VerifyError::UnknownKid);
            }
        }

        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[self.config.issuer.as_str()]);
        // jsonwebtoken 的 set_audience 要求至少一个匹配；我们传默认 audience
        let aud_refs: Vec<&str> = self
            .config
            .default_audience
            .iter()
            .map(|s| s.as_str())
            .collect();
        validation.set_audience(&aud_refs);
        validation.leeway = 5;

        let data = decode::<UnifiedTokenClaims>(token, &self.decoding_key, &validation).map_err(
            |e| match e.kind() {
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
            },
        )?;

        if data.claims.token_type != expected_typ {
            return Err(VerifyError::Invalid);
        }
        Ok(data.claims)
    }
}

/// 验签错误码（与 spec §11.6 / §6.1 introspect 的 `reason` 字段对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// 签名 / 格式 / typ 错误
    Invalid,
    /// `exp < now`
    Expired,
    /// `kid` 与当前签名 key 不匹配（Phase A 内只有一把 key）
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

    // 优先 PKCS#8 ("BEGIN PUBLIC KEY")，再尝试 PKCS#1 ("BEGIN RSA PUBLIC KEY")
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

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1::EncodeRsaPrivateKey;
    use rsa::pkcs1::EncodeRsaPublicKey;
    use rsa::RsaPrivateKey;
    use std::io::Write;

    /// 生成一对临时 key，写到 /tmp 然后塞进 RsaJwtConfig。
    fn make_key_pair() -> (tempdir_files::TempPaths, RsaJwtConfig) {
        let mut rng = rand::thread_rng();
        let private = RsaPrivateKey::new(&mut rng, 2048).expect("gen");
        let public = RsaPublicKey::from(&private);
        let priv_pem = private.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();
        let pub_pem = public.to_pkcs1_pem(rsa::pkcs1::LineEnding::LF).unwrap();

        let dir = std::env::temp_dir().join(format!(
            "privchat-rsajwt-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let priv_path = dir.join("v1.pem");
        let pub_path = dir.join("v1.pub.pem");
        let mut f = std::fs::File::create(&priv_path).unwrap();
        f.write_all(priv_pem.as_bytes()).unwrap();
        let mut f = std::fs::File::create(&pub_path).unwrap();
        f.write_all(pub_pem.as_bytes()).unwrap();

        let cfg = RsaJwtConfig {
            private_key_path: priv_path.to_string_lossy().into_owned(),
            public_key_path: pub_path.to_string_lossy().into_owned(),
            kid: "test".to_string(),
            access_ttl_secs: 2,        // 短 TTL 方便 expired 测试
            refresh_ttl_secs: 3600,
            issuer: "privchat-server".to_string(),
            default_audience: vec![
                "privchat-application".to_string(),
                "privchat-server".to_string(),
            ],
        };
        (tempdir_files::TempPaths { dir }, cfg)
    }

    /// 临时密钥目录的 RAII 清理。
    mod tempdir_files {
        use std::path::PathBuf;
        pub struct TempPaths {
            pub dir: PathBuf,
        }
        impl Drop for TempPaths {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
    }

    fn make_issue_params() -> IssueClaims {
        IssueClaims {
            user_id: 1234567890,
            device_id: "test-dev-1".to_string(),
            session_version: 1,
            scope: vec!["user".to_string()],
            audience: vec![
                "privchat-application".to_string(),
                "privchat-server".to_string(),
            ],
            business_system_id: "privchat-application".to_string(),
            app_id: "ios".to_string(),
        }
    }

    #[test]
    fn issue_and_verify_access_round_trip() {
        let (_t, cfg) = make_key_pair();
        let svc = RsaJwtService::from_config(cfg).unwrap();

        let issued = svc.issue_access(make_issue_params()).unwrap();
        assert_eq!(issued.claims.token_type, TOKEN_TYPE_ACCESS);
        assert_eq!(issued.claims.iss, "privchat-server");
        assert!(issued.claims.aud.contains(&"privchat-application".to_string()));
        assert!(issued.claims.aud.contains(&"privchat-server".to_string()));
        assert_eq!(issued.claims.session_version, 1);

        let verified = svc.verify(&issued.token, TOKEN_TYPE_ACCESS).unwrap();
        assert_eq!(verified.sub, "1234567890");
        assert_eq!(verified.device_id, "test-dev-1");
        assert_eq!(verified.scope, vec!["user".to_string()]);
    }

    #[test]
    fn refresh_token_typ_check() {
        let (_t, cfg) = make_key_pair();
        let svc = RsaJwtService::from_config(cfg).unwrap();
        let access = svc.issue_access(make_issue_params()).unwrap();

        // 用 access token 假装 refresh 验证：应被 typ 校验拒绝
        let err = svc.verify(&access.token, TOKEN_TYPE_REFRESH).unwrap_err();
        assert_eq!(err, VerifyError::Invalid);
    }

    #[test]
    fn verify_returns_expired_after_ttl() {
        let (_t, cfg) = make_key_pair(); // access_ttl_secs = 2
        let svc = RsaJwtService::from_config(cfg).unwrap();
        let issued = svc.issue_access(make_issue_params()).unwrap();
        std::thread::sleep(std::time::Duration::from_secs(8)); // 2s TTL + 5s leeway + buffer
        let err = svc.verify(&issued.token, TOKEN_TYPE_ACCESS).unwrap_err();
        assert_eq!(err, VerifyError::Expired);
    }

    #[test]
    fn verify_returns_invalid_for_tampered() {
        let (_t, cfg) = make_key_pair();
        let svc = RsaJwtService::from_config(cfg).unwrap();
        let issued = svc.issue_access(make_issue_params()).unwrap();
        // 改最后一个字符破坏签名
        let mut tampered = issued.token.clone();
        let last = tampered.pop().unwrap();
        tampered.push(if last == 'A' { 'B' } else { 'A' });
        let err = svc.verify(&tampered, TOKEN_TYPE_ACCESS).unwrap_err();
        assert_eq!(err, VerifyError::Invalid);
    }

    #[test]
    fn verify_unknown_kid_returns_unknown_kid() {
        let (_t, mut cfg) = make_key_pair();
        cfg.kid = "kid-A".to_string();
        let svc_a = RsaJwtService::from_config(cfg.clone()).unwrap();
        let issued = svc_a.issue_access(make_issue_params()).unwrap();

        // 用同样 key 但不同 kid 的 service 验签：unknown_kid
        cfg.kid = "kid-B".to_string();
        let svc_b = RsaJwtService::from_config(cfg).unwrap();
        let err = svc_b.verify(&issued.token, TOKEN_TYPE_ACCESS).unwrap_err();
        assert_eq!(err, VerifyError::UnknownKid);
    }

    #[test]
    fn jwks_emits_rsa_kid_alg() {
        let (_t, cfg) = make_key_pair();
        let svc = RsaJwtService::from_config(cfg).unwrap();
        let jwks = svc.jwks();
        assert_eq!(jwks.keys.len(), 1);
        let k = &jwks.keys[0];
        assert_eq!(k.kid, "test");
        assert_eq!(k.alg, "RS256");
        assert_eq!(k.kty, "RSA");
        assert_eq!(k.use_, "sig");
        // n / e 是 base64url-no-pad
        assert!(!k.n.is_empty());
        assert!(!k.e.is_empty());
        assert!(!k.n.contains('='));
    }

    #[test]
    fn from_config_fails_when_paths_missing() {
        let cfg = RsaJwtConfig {
            private_key_path: "/tmp/no-such-priv.pem".to_string(),
            public_key_path: "/tmp/no-such-pub.pem".to_string(),
            ..RsaJwtConfig::default()
        };
        let err = RsaJwtService::from_config(cfg).unwrap_err();
        assert!(format!("{}", err).contains("读私钥失败"));
    }
}
