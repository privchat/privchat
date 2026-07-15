// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `privchat_refresh_tokens` 表 DAO（spec TOKEN_UNIFICATION_SPEC v1.3 §11.2）。
//!
//! 表结构见 `migrations/015_v1_3_create_refresh_tokens.sql`。
//!
//! 关键约束：
//! - `token_hash` 是 SHA-256 hex；**永不**存明文 refresh JWT
//! - 验证流程：解 JWT → 取 `jti` → [`Self::find_active_by_jti`] →
//!   外部用恒定时间比对 `token_hash` → 检查 `revoked_at IS NULL` 且 `expires_at > now`
//! - revoke 路径：[`Self::revoke_by_jti`] / [`Self::revoke_by_token_hash`] /
//!   [`Self::revoke_by_user_device`] 都只修改 `revoked_at` + `revoke_reason`，行不删

use std::sync::Arc;

use anyhow::Result;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RefreshTokenRecord {
    pub jti: String,
    pub user_id: i64,
    pub device_id: String,
    pub token_hash: String,
    pub session_version: i64,
    pub expires_at: i64,
    pub revoked_at: Option<i64>,
    pub revoke_reason: Option<String>,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

impl RefreshTokenRecord {
    /// 是否已 revoke（`revoked_at` 非 NULL）。
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }

    /// 是否过期（按 unix ms 比较）。
    pub fn is_expired(&self, now_ms: i64) -> bool {
        now_ms >= self.expires_at
    }
}

/// 计算 refresh JWT 的 SHA-256 hex（lowercase）作为 `token_hash`。
pub fn hash_refresh_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

#[derive(Clone)]
pub struct RefreshTokenRepository {
    pool: Arc<PgPool>,
}

impl RefreshTokenRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 插入一条 refresh token 记录（spec §11.2 phase A）。
    ///
    /// `token` 是签好的 refresh JWT 明文，本方法**只**取它的 SHA-256 hex 落库；
    /// 调用方拿到 `Ok(())` 后**必须**用同一字符串返给客户端，server 端不再持有明文。
    pub async fn insert(
        &self,
        jti: &str,
        user_id: u64,
        device_id: &str,
        token: &str,
        session_version: i64,
        expires_at_ms: i64,
        created_at_ms: i64,
    ) -> Result<()> {
        let token_hash = hash_refresh_token(token);
        sqlx::query(
            r#"
            INSERT INTO privchat_refresh_tokens
                (jti, user_id, device_id, token_hash, session_version,
                 expires_at, revoked_at, revoke_reason, created_at, last_used_at)
            VALUES ($1, $2, $3, $4, $5, $6, NULL, NULL, $7, NULL)
            "#,
        )
        .bind(jti)
        .bind(user_id as i64)
        .bind(device_id)
        .bind(token_hash)
        .bind(session_version)
        .bind(expires_at_ms)
        .bind(created_at_ms)
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// 仅按 `jti` 查找记录；不过滤 revoked / expired，由调用方判断。
    pub async fn find_by_jti(&self, jti: &str) -> Result<Option<RefreshTokenRecord>> {
        let row = sqlx::query_as::<_, RefreshTokenRecord>(
            r#"
            SELECT jti, user_id, device_id, token_hash, session_version,
                   expires_at, revoked_at, revoke_reason, created_at, last_used_at
            FROM privchat_refresh_tokens
            WHERE jti = $1
            "#,
        )
        .bind(jti)
        .fetch_optional(self.pool.as_ref())
        .await?;
        Ok(row)
    }

    /// 标记 refresh 已使用（`last_used_at = now_ms`）；非 rotation 路径下用。
    pub async fn touch_last_used(&self, jti: &str, now_ms: i64) -> Result<()> {
        sqlx::query("UPDATE privchat_refresh_tokens SET last_used_at = $1 WHERE jti = $2")
            .bind(now_ms)
            .bind(jti)
            .execute(self.pool.as_ref())
            .await?;
        Ok(())
    }

    /// 按 `jti` revoke。返回受影响行数（0 = miss / 已 revoke 不重复改）。
    pub async fn revoke_by_jti(&self, jti: &str, reason: &str, now_ms: i64) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE privchat_refresh_tokens
            SET revoked_at = $1, revoke_reason = $2
            WHERE jti = $3 AND revoked_at IS NULL
            "#,
        )
        .bind(now_ms)
        .bind(reason)
        .bind(jti)
        .execute(self.pool.as_ref())
        .await?;
        Ok(res.rows_affected())
    }

    /// 按 `token_hash` revoke（调用方先 SHA-256 哈希明文 token）。
    pub async fn revoke_by_token_hash(
        &self,
        token_hash: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE privchat_refresh_tokens
            SET revoked_at = $1, revoke_reason = $2
            WHERE token_hash = $3 AND revoked_at IS NULL
            "#,
        )
        .bind(now_ms)
        .bind(reason)
        .bind(token_hash)
        .execute(self.pool.as_ref())
        .await?;
        Ok(res.rows_affected())
    }

    /// 按 `(user_id, device_id)` revoke 该设备的全部活跃 refresh 记录。
    pub async fn revoke_by_user_device(
        &self,
        user_id: u64,
        device_id: &str,
        reason: &str,
        now_ms: i64,
    ) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE privchat_refresh_tokens
            SET revoked_at = $1, revoke_reason = $2
            WHERE user_id = $3 AND device_id = $4 AND revoked_at IS NULL
            "#,
        )
        .bind(now_ms)
        .bind(reason)
        .bind(user_id as i64)
        .bind(device_id)
        .execute(self.pool.as_ref())
        .await?;
        Ok(res.rows_affected())
    }

    /// 按 `user_id` revoke 全部活跃 refresh 记录（配合 `bumpSessions` 用）。
    pub async fn revoke_by_user(&self, user_id: u64, reason: &str, now_ms: i64) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE privchat_refresh_tokens
            SET revoked_at = $1, revoke_reason = $2
            WHERE user_id = $3 AND revoked_at IS NULL
            "#,
        )
        .bind(now_ms)
        .bind(reason)
        .bind(user_id as i64)
        .execute(self.pool.as_ref())
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_refresh_token_is_deterministic_lowercase_hex() {
        let h = hash_refresh_token("hello");
        // SHA256("hello") = 2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
        assert_eq!(
            h,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        // 重复调用稳定
        assert_eq!(h, hash_refresh_token("hello"));
        // 长度固定 64 hex chars
        assert_eq!(h.len(), 64);
    }

    #[test]
    fn record_is_revoked_and_expired() {
        let mut r = RefreshTokenRecord {
            jti: "j".into(),
            user_id: 1,
            device_id: "d".into(),
            token_hash: "h".into(),
            session_version: 1,
            expires_at: 1000,
            revoked_at: None,
            revoke_reason: None,
            created_at: 0,
            last_used_at: None,
        };
        assert!(!r.is_revoked());
        assert!(!r.is_expired(999));
        assert!(r.is_expired(1000));
        assert!(r.is_expired(2000));

        r.revoked_at = Some(500);
        assert!(r.is_revoked());
    }
}
