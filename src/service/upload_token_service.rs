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

/// 上传 Token 服务
pub struct UploadTokenService {
    /// Token 存储（token -> UploadToken）
    tokens: Arc<RwLock<HashMap<String, UploadToken>>>,
}

impl UploadTokenService {
    /// 创建新的 UploadTokenService
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
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
    ) -> Result<UploadToken> {
        let token = UploadToken::new(user_id, file_type, max_size, business_type, filename);

        // 存储 token
        self.tokens
            .write()
            .await
            .insert(token.token.clone(), token.clone());

        info!(
            "🎫 生成上传 token: {} (用户: {}, 类型: {}, 最大: {} bytes, 业务: {})",
            token.token,
            token.user_id,
            token.file_type.as_str(),
            token.max_size,
            token.business_type
        );

        Ok(token)
    }

    /// 验证 token 有效性
    pub async fn validate_token(&self, token: &str) -> Result<UploadToken> {
        let tokens = self.tokens.read().await;

        match tokens.get(token) {
            Some(upload_token) => {
                if upload_token.is_valid() {
                    debug!("✅ Token 验证通过: {}", token);
                    Ok(upload_token.clone())
                } else {
                    warn!(
                        "❌ Token 已失效: {} (已使用: {}, 过期: {})",
                        token,
                        upload_token.used,
                        Utc::now() >= upload_token.expires_at
                    );
                    Err(ServerError::InvalidToken)
                }
            }
            None => {
                warn!("❌ Token 不存在: {}", token);
                Err(ServerError::InvalidToken)
            }
        }
    }

    /// 标记 token 已使用
    pub async fn mark_token_used(&self, token: &str) -> Result<()> {
        let mut tokens = self.tokens.write().await;

        match tokens.get_mut(token) {
            Some(upload_token) => {
                if upload_token.is_valid() {
                    upload_token.mark_used();
                    info!("✅ Token 标记为已使用: {}", token);
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
        let mut tokens = self.tokens.write().await;

        match tokens.remove(token) {
            Some(_) => {
                debug!("🗑️ Token 已删除: {}", token);
                Ok(())
            }
            None => Err(ServerError::InvalidToken),
        }
    }

    /// 清理过期的 token（定期调用）
    pub async fn cleanup_expired_tokens(&self) {
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
                "user1".to_string(),
                FileType::Image,
                10485760, // 10MB
                "message".to_string(),
                Some("test.jpg".to_string()),
            )
            .await
            .unwrap();

        // 验证 token
        let validated = service.validate_token(&token.token).await.unwrap();
        assert_eq!(validated.user_id, "user1");
        assert_eq!(validated.file_type, FileType::Image);
        assert_eq!(validated.business_type, "message");
        assert!(validated.is_valid());
    }

    #[tokio::test]
    async fn test_mark_token_used() {
        let service = UploadTokenService::new();

        let token = service
            .generate_token(
                "user1".to_string(),
                FileType::Image,
                10485760,
                "message".to_string(),
                None,
            )
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
}
