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

//! 上传 Token 服务
//!
//! 管理临时上传 token，用于文件上传的权限控制

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{Result, ServerError};
use crate::service::file_service::FileType;

/// 这张 token 是干什么用的。
///
/// 🔴 一次性 token 只保证「同一入口不能用两次」，挡不住**两个入口各用一次**：
/// claim 与实体上传都是先校验后消费，并发时可以双双通过，最后留下一条 claim 行
/// 和一条上传行。用途签进 token，两个入口各自拒绝不属于自己的那种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadTokenPurpose {
    /// 预检未命中：这张 token 只能拿去传字节。
    Upload,
    /// 预检命中：这张 token 只能拿去换自己的 file_id，不能再传字节。
    ClaimExisting,
}

impl Default for UploadTokenPurpose {
    /// 老 token（Redis 里没有这个字段）按实体上传处理，兼容滚动升级。
    fn default() -> Self {
        Self::Upload
    }
}

/// prepare 阶段声明的文件身份，签进 token 后在完成时逐项复核。
#[derive(Debug, Clone, Default)]
pub struct UploadIdentity {
    pub sha256: Option<String>,
    pub declared_size: Option<i64>,
    pub mime_type: Option<String>,
    pub transform_version: i32,
}

/// 上传 Token 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadToken {
    /// Token（UUID）
    pub token: String,
    /// 发起上传的用户 ID
    pub user_id: u64,
    /// 文件类型
    pub file_type: FileType,
    /// 允许的最大文件大小（字节）
    pub max_size: i64,
    /// 业务类型（avatar/message/group_file/...）
    pub business_type: String,
    /// 原始文件名（可选）
    pub filename: Option<String>,
    /// 客户端在 prepare 阶段声明的最终内容摘要（SHA-256 十六进制）。
    ///
    /// 🔴 文件身份必须**绑在 token 里**，完成时逐项复核。只信请求参数的话，
    /// 客户端可以在 prepare 与 upload 之间换掉摘要、大小或处理版本——
    /// 那样秒传判定用的是一组参数，落库用的是另一组。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 声明的精确大小（字节）。`max_size` 是上限，这是「就该是这么大」。
    #[serde(default)]
    pub declared_size: Option<i64>,
    /// 声明的 MIME。
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 产出这份字节的客户端处理版本。
    #[serde(default)]
    pub transform_version: i32,
    /// 这张 token 的用途，见 [`UploadTokenPurpose`]。
    #[serde(default)]
    pub purpose: UploadTokenPurpose,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 过期时间（默认 5 分钟）
    pub expires_at: DateTime<Utc>,
    /// 是否已使用（一次性）
    pub used: bool,
}

impl UploadToken {
    /// 创建新的上传 token
    pub fn new(
        user_id: u64,
        file_type: FileType,
        max_size: i64,
        business_type: String,
        filename: Option<String>,
        identity: UploadIdentity,
        purpose: UploadTokenPurpose,
    ) -> Self {
        let now = Utc::now();
        let token = Uuid::new_v4().to_string();

        Self {
            token,
            user_id,
            file_type,
            max_size,
            business_type,
            filename,
            sha256: identity.sha256,
            declared_size: identity.declared_size,
            mime_type: identity.mime_type,
            transform_version: identity.transform_version,
            purpose,
            created_at: now,
            expires_at: now + Duration::minutes(5), // 5 分钟过期
            used: false,
        }
    }

    /// 检查 token 是否有效
    pub fn is_valid(&self) -> bool {
        !self.used && Utc::now() < self.expires_at
    }

    /// 标记 token 已使用
    pub fn mark_used(&mut self) {
        self.used = true;
    }
}

/// 日志用 token 脱敏：只保留前 8 位（P0-10：上传 token 不落明文日志）。
fn redact(token: &str) -> String {
    format!("{}…", token.chars().take(8).collect::<String>())
}

/// 派生 `upload_id` 时的域分隔前缀。
///
/// 加前缀是为了让这个哈希只在「旧 token → upload_id」这一个用途下有意义：
/// 不加的话，任何别处对同一字符串取 SHA-256 的地方都会算出同一个值。
const LEGACY_UPLOAD_ID_DOMAIN: &[u8] = b"legacy-upload-id\0";

/// 旧 UUID token 没有 `upload_id`，但会话目录、模式锁和 `upload_completion_key`
/// 全都以它为轴。这里从 token 稳定派生一个。
///
/// 🔴 **不能直接拿 token 当 `upload_id`**：它是 bearer 凭证，而 `upload_id` 会成为
/// 目录名和日志字段（RESUMABLE_UPLOAD_SPEC §5.2.3 / §7）。
///
/// 纯函数 ⇒ 同一张旧 token 的每次重试都落到同一个会话目录，模式锁与完成幂等照常生效。
pub fn derive_legacy_upload_id(token: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(LEGACY_UPLOAD_ID_DOMAIN);
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 上传凭证验证后的统一结果。
///
/// 迁移期同时存在两种 token 格式（自包含签名 token 与旧的 Redis UUID token）。
/// 验证器只输出这一个模型，**调用方不再判断 token 是什么格式**——否则模式锁、
/// 会话目录、完成幂等每一处都要各写一遍分叉。
#[derive(Debug, Clone)]
pub struct ValidatedUploadToken {
    /// 这次上传的唯一标识：会话目录名、模式锁与 `upload_completion_key` 的轴。
    ///
    /// 签名 token 直接读签进去的值；旧 UUID token 走 [`derive_legacy_upload_id`]。
    pub upload_id: String,
    pub user_id: u64,
    pub purpose: UploadTokenPurpose,
    pub file_type: FileType,
    pub max_size: i64,
    pub business_type: String,
    pub filename: Option<String>,
    /// prepare 阶段冻结的文件身份，完成时逐项复核。
    pub sha256: Option<String>,
    pub declared_size: Option<i64>,
    pub mime_type: Option<String>,
    pub transform_version: i32,
    pub expires_at: DateTime<Utc>,
}

impl ValidatedUploadToken {
    /// 请求开始时是否仍然有效。
    ///
    /// 只看过期时间：一次性消费语义正在被移除（spec §5.2.5），
    /// 重放由 `upload_completion_key` 与模式锁承担。
    pub fn is_fresh(&self) -> bool {
        Utc::now() < self.expires_at
    }
}

impl From<&UploadToken> for ValidatedUploadToken {
    /// 旧 UUID token 的投影：`upload_id` 由 token 派生，其余字段原样带过。
    fn from(t: &UploadToken) -> Self {
        Self {
            upload_id: derive_legacy_upload_id(&t.token),
            user_id: t.user_id,
            purpose: t.purpose,
            file_type: t.file_type.clone(),
            max_size: t.max_size,
            business_type: t.business_type.clone(),
            filename: t.filename.clone(),
            sha256: t.sha256.clone(),
            declared_size: t.declared_size,
            mime_type: t.mime_type.clone(),
            transform_version: t.transform_version,
            expires_at: t.expires_at,
        }
    }
}

/// 上传 Token 服务
///
/// P0-10：优先走 Redis（key `upload_token:{token}`，SETEX 到期自灭，GETDEL 原子
/// 消费保证一次性语义跨实例成立）；无 Redis 时回退进程内存（单实例/测试）。
pub struct UploadTokenService {
    /// 内存回退存储（token -> UploadToken）
    tokens: Arc<RwLock<HashMap<String, UploadToken>>>,
    redis: Option<Arc<crate::infra::redis::RedisClient>>,
}

const REDIS_KEY_PREFIX: &str = "upload_token:";

impl UploadTokenService {
    /// 创建新的 UploadTokenService（内存模式，测试/无 Redis 场景）
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            redis: None,
        }
    }

    /// 带 Redis 后端创建（生产路径）：token 状态跨实例可见。
    pub fn new_with_redis(redis: Arc<crate::infra::redis::RedisClient>) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            redis: Some(redis),
        }
    }

    /// 生成上传 token
    pub async fn generate_token(
        &self,
        user_id: u64,
        file_type: FileType,
        max_size: i64,
        business_type: String,
        filename: Option<String>,
        identity: UploadIdentity,
        purpose: UploadTokenPurpose,
    ) -> Result<UploadToken> {
        let token = UploadToken::new(
            user_id,
            file_type,
            max_size,
            business_type,
            filename,
            identity,
            purpose,
        );

        if let Some(redis) = &self.redis {
            let ttl = (token.expires_at - Utc::now()).num_seconds().max(1) as usize;
            let payload = serde_json::to_string(&token)
                .map_err(|e| ServerError::Internal(format!("序列化 upload token 失败: {}", e)))?;
            redis
                .setex(&format!("{REDIS_KEY_PREFIX}{}", token.token), ttl, &payload)
                .await?;
        } else {
            self.tokens
                .write()
                .await
                .insert(token.token.clone(), token.clone());
        }

        info!(
            "🎫 生成上传 token: {} (用户: {}, 类型: {}, 最大: {} bytes, 业务: {})",
            redact(&token.token),
            token.user_id,
            token.file_type.as_str(),
            token.max_size,
            token.business_type
        );

        Ok(token)
    }

    /// 验证 token 有效性
    pub async fn validate_token(&self, token: &str) -> Result<UploadToken> {
        if let Some(redis) = &self.redis {
            let payload = redis.get(&format!("{REDIS_KEY_PREFIX}{token}")).await?;
            return match payload.and_then(|p| serde_json::from_str::<UploadToken>(&p).ok()) {
                Some(upload_token) if upload_token.is_valid() => {
                    debug!("✅ Token 验证通过: {}", redact(token));
                    Ok(upload_token)
                }
                _ => {
                    warn!("❌ Token 不存在或已失效: {}", redact(token));
                    Err(ServerError::InvalidToken)
                }
            };
        }

        let tokens = self.tokens.read().await;
        match tokens.get(token) {
            Some(upload_token) => {
                if upload_token.is_valid() {
                    debug!("✅ Token 验证通过: {}", redact(token));
                    Ok(upload_token.clone())
                } else {
                    warn!(
                        "❌ Token 已失效: {} (已使用: {}, 过期: {})",
                        redact(token),
                        upload_token.used,
                        Utc::now() >= upload_token.expires_at
                    );
                    Err(ServerError::InvalidToken)
                }
            }
            None => {
                warn!("❌ Token 不存在: {}", redact(token));
                Err(ServerError::InvalidToken)
            }
        }
    }

    /// 标记 token 已使用（一次性消费）。
    /// Redis 路径用 GETDEL 原子消费：并发/跨实例重复使用只有一个赢家。
    pub async fn mark_token_used(&self, token: &str) -> Result<()> {
        if let Some(redis) = &self.redis {
            return match redis.getdel(&format!("{REDIS_KEY_PREFIX}{token}")).await? {
                Some(_) => {
                    info!("✅ Token 已消费: {}", redact(token));
                    Ok(())
                }
                None => {
                    warn!("❌ Token 已被使用或过期: {}", redact(token));
                    Err(ServerError::InvalidToken)
                }
            };
        }

        let mut tokens = self.tokens.write().await;
        match tokens.get_mut(token) {
            Some(upload_token) => {
                if upload_token.is_valid() {
                    upload_token.mark_used();
                    info!("✅ Token 标记为已使用: {}", redact(token));
                    Ok(())
                } else {
                    Err(ServerError::InvalidToken)
                }
            }
            None => Err(ServerError::InvalidToken),
        }
    }

    /// 删除 token（清理）
    pub async fn remove_token(&self, token: &str) -> Result<()> {
        if let Some(redis) = &self.redis {
            redis.del(&format!("{REDIS_KEY_PREFIX}{token}")).await?;
            return Ok(());
        }
        let mut tokens = self.tokens.write().await;
        match tokens.remove(token) {
            Some(_) => {
                debug!("🗑️ Token 已删除: {}", redact(token));
                Ok(())
            }
            None => Err(ServerError::InvalidToken),
        }
    }

    /// 清理过期的 token（定期调用）。Redis 路径由 SETEX TTL 自灭，无需清理。
    pub async fn cleanup_expired_tokens(&self) {
        if self.redis.is_some() {
            return;
        }
        let mut tokens = self.tokens.write().await;
        let now = Utc::now();

        let expired_tokens: Vec<String> = tokens
            .iter()
            .filter(|(_, token)| token.expires_at < now)
            .map(|(key, _)| key.clone())
            .collect();

        for token in &expired_tokens {
            tokens.remove(token);
        }

        if !expired_tokens.is_empty() {
            info!("🧹 清理过期 token: {} 个", expired_tokens.len());
        }
    }

    /// 获取当前 token 数量（用于监控）
    pub async fn token_count(&self) -> usize {
        self.tokens.read().await.len()
    }
}

impl Default for UploadTokenService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_and_validate_token() {
        let service = UploadTokenService::new();

        // 生成 token
        let token = service
            .generate_token(
                1001,
                FileType::Image,
                10485760, // 10MB
                "message".to_string(),
                Some("test.jpg".to_string()),
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
            )
            .await
            .unwrap();

        // 验证 token
        let validated = service.validate_token(&token.token).await.unwrap();
        assert_eq!(validated.user_id, 1001);
        assert_eq!(validated.file_type, FileType::Image);
        assert_eq!(validated.business_type, "message");
        assert!(validated.is_valid());
    }

    #[tokio::test]
    async fn test_mark_token_used() {
        let service = UploadTokenService::new();

        let token = service
            .generate_token(1001, FileType::Image, 10485760, "message".to_string(), None, UploadIdentity::default(), UploadTokenPurpose::Upload)
            .await
            .unwrap();

        // 标记已使用
        service.mark_token_used(&token.token).await.unwrap();

        // 再次验证应该失败
        let result = service.validate_token(&token.token).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_token() {
        let service = UploadTokenService::new();

        let result = service.validate_token("invalid-token").await;
        assert!(result.is_err());
    }

    /// 会话目录、模式锁和完成幂等都按 `upload_id` 定位。旧 token 的重试必须
    /// 落回同一个目录，否则每次重试都会开一个新会话，断点续传无从谈起。
    #[test]
    fn the_same_legacy_token_always_names_the_same_upload() {
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        assert_eq!(derive_legacy_upload_id(token), derive_legacy_upload_id(token));
    }

    #[test]
    fn two_legacy_tokens_never_share_an_upload_directory() {
        let a = derive_legacy_upload_id("b3f1a2c4-0000-4000-8000-000000000001");
        let b = derive_legacy_upload_id("b3f1a2c4-0000-4000-8000-000000000002");
        assert_ne!(a, b);
    }

    /// `upload_id` 会成为磁盘上的目录名和日志字段；token 是 bearer 凭证。
    /// 派生值一旦等于（或包含）token 本身，凭证就随着目录列表泄露了。
    #[test]
    fn the_upload_id_never_leaks_the_bearer_token() {
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        let id = derive_legacy_upload_id(token);
        assert_ne!(id, token);
        assert!(!id.contains(token));
        // 目录名安全：只能是十六进制。
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id.len(), 64);
    }

    /// 域分隔的意义：别处对同一字符串取裸 SHA-256 时，不会算出同一个 upload_id。
    #[test]
    fn the_derivation_is_domain_separated_from_a_bare_digest() {
        use sha2::Digest as _;
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        let bare = hex::encode(sha2::Sha256::digest(token.as_bytes()));
        assert_ne!(derive_legacy_upload_id(token), bare);
    }

    /// 统一模型必须把冻结的文件身份原样带过：complete 时要拿它逐项复核，
    /// 漏一个字段就等于那一项没有被校验。
    #[tokio::test]
    async fn the_validated_projection_carries_the_frozen_identity() {
        let service = UploadTokenService::new();
        let identity = UploadIdentity {
            sha256: Some("a".repeat(64)),
            declared_size: Some(4096),
            mime_type: Some("image/png".to_string()),
            transform_version: 7,
        };
        let token = service
            .generate_token(
                1001,
                FileType::Image,
                10485760,
                "message".to_string(),
                Some("holiday.png".to_string()),
                identity,
                UploadTokenPurpose::ClaimExisting,
            )
            .await
            .unwrap();

        let validated = ValidatedUploadToken::from(&token);

        assert_eq!(validated.upload_id, derive_legacy_upload_id(&token.token));
        assert_eq!(validated.user_id, 1001);
        assert_eq!(validated.purpose, UploadTokenPurpose::ClaimExisting);
        assert_eq!(validated.business_type, "message");
        assert_eq!(validated.filename.as_deref(), Some("holiday.png"));
        assert_eq!(validated.sha256, Some("a".repeat(64)));
        assert_eq!(validated.declared_size, Some(4096));
        assert_eq!(validated.mime_type.as_deref(), Some("image/png"));
        assert_eq!(validated.transform_version, 7);
        assert_eq!(validated.max_size, 10485760);
        assert!(validated.is_fresh());
    }
}
