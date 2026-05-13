// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `privchat_bot_follow` 表 DAO（spec `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC` §4）。
//!
//! 表结构见 `migrations/017_privchat_bot_follow.sql`。
//!
//! 关键约束：
//! - `(user_id, bot_user_id)` UNIQUE
//! - `status` 切换不删行；followed (1) ↔ unfollowed (0)
//! - `channel_id` 在第一次 follow 时分配，后续 status 切换沿用同一 channel_id

use std::sync::Arc;

use anyhow::Result;
use sqlx::PgPool;

pub const STATUS_FOLLOWED: i16 = 1;
pub const STATUS_UNFOLLOWED: i16 = 0;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct BotFollowRecord {
    pub id: i64,
    pub user_id: i64,
    pub bot_user_id: i64,
    pub channel_id: i64,
    pub status: i16,
    pub followed_at: i64,
    pub unfollowed_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl BotFollowRecord {
    pub fn is_followed(&self) -> bool {
        self.status == STATUS_FOLLOWED
    }
}

#[derive(Clone)]
pub struct BotFollowRepository {
    pool: Arc<PgPool>,
}

/// Result of [`BotFollowRepository::upsert_followed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowUpsertOutcome {
    /// Brand new row inserted (first time this user follows this bot).
    InsertedNew,
    /// Row existed and was already `status=followed` — nothing changed.
    AlreadyFollowed,
    /// Row existed at `status=unfollowed` and was flipped back to followed.
    Revived,
}

impl FollowUpsertOutcome {
    /// `true` for the two "this counts as a fresh follow" cases (insert + revive).
    /// Used in spec §3.1 step 4 to set the response field `created`.
    pub fn is_created(self) -> bool {
        matches!(self, Self::InsertedNew | Self::Revived)
    }
}

impl BotFollowRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 按 (user_id, bot_user_id) 取一行；不区分 status。
    pub async fn find(
        &self,
        user_id: u64,
        bot_user_id: u64,
    ) -> Result<Option<BotFollowRecord>> {
        let row = sqlx::query_as::<_, BotFollowRecord>(
            r#"
            SELECT id, user_id, bot_user_id, channel_id, status,
                   followed_at, unfollowed_at, created_at, updated_at
            FROM privchat_bot_follow
            WHERE user_id = $1 AND bot_user_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(bot_user_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await?;
        Ok(row)
    }

    /// Upsert：行不存在则 insert，存在但 unfollowed 则 revive，已 followed 则 no-op。
    /// 返回结果用于决定 spec §3.1 中 `created` 字段。
    pub async fn upsert_followed(
        &self,
        user_id: u64,
        bot_user_id: u64,
        channel_id: u64,
        now_ms: i64,
    ) -> Result<FollowUpsertOutcome> {
        let mut tx = self.pool.begin().await?;

        let existing = sqlx::query_as::<_, BotFollowRecord>(
            r#"
            SELECT id, user_id, bot_user_id, channel_id, status,
                   followed_at, unfollowed_at, created_at, updated_at
            FROM privchat_bot_follow
            WHERE user_id = $1 AND bot_user_id = $2
            FOR UPDATE
            "#,
        )
        .bind(user_id as i64)
        .bind(bot_user_id as i64)
        .fetch_optional(&mut *tx)
        .await?;

        let outcome = match existing {
            Some(rec) if rec.status == STATUS_FOLLOWED => FollowUpsertOutcome::AlreadyFollowed,
            Some(_) => {
                sqlx::query(
                    r#"
                    UPDATE privchat_bot_follow
                    SET status = $1,
                        followed_at = $2,
                        unfollowed_at = NULL,
                        updated_at = $2
                    WHERE user_id = $3 AND bot_user_id = $4
                    "#,
                )
                .bind(STATUS_FOLLOWED)
                .bind(now_ms)
                .bind(user_id as i64)
                .bind(bot_user_id as i64)
                .execute(&mut *tx)
                .await?;
                FollowUpsertOutcome::Revived
            }
            None => {
                sqlx::query(
                    r#"
                    INSERT INTO privchat_bot_follow
                        (user_id, bot_user_id, channel_id, status,
                         followed_at, unfollowed_at, created_at, updated_at)
                    VALUES ($1, $2, $3, $4, $5, NULL, $5, $5)
                    "#,
                )
                .bind(user_id as i64)
                .bind(bot_user_id as i64)
                .bind(channel_id as i64)
                .bind(STATUS_FOLLOWED)
                .bind(now_ms)
                .execute(&mut *tx)
                .await?;
                FollowUpsertOutcome::InsertedNew
            }
        };

        tx.commit().await?;
        Ok(outcome)
    }

    /// 把已存在的 followed 行翻成 unfollowed。
    /// 返回受影响行数：0 = 行不存在 / 已 unfollowed，1 = 翻转成功。
    pub async fn set_unfollowed(
        &self,
        user_id: u64,
        bot_user_id: u64,
        now_ms: i64,
    ) -> Result<u64> {
        let res = sqlx::query(
            r#"
            UPDATE privchat_bot_follow
            SET status = $1,
                unfollowed_at = $2,
                updated_at = $2
            WHERE user_id = $3
              AND bot_user_id = $4
              AND status = $5
            "#,
        )
        .bind(STATUS_UNFOLLOWED)
        .bind(now_ms)
        .bind(user_id as i64)
        .bind(bot_user_id as i64)
        .bind(STATUS_FOLLOWED)
        .execute(self.pool.as_ref())
        .await?;
        Ok(res.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_created_flag() {
        assert!(FollowUpsertOutcome::InsertedNew.is_created());
        assert!(FollowUpsertOutcome::Revived.is_created());
        assert!(!FollowUpsertOutcome::AlreadyFollowed.is_created());
    }

    #[test]
    fn record_is_followed_flag() {
        let mut r = BotFollowRecord {
            id: 1,
            user_id: 1,
            bot_user_id: 2,
            channel_id: 99,
            status: STATUS_FOLLOWED,
            followed_at: 0,
            unfollowed_at: None,
            created_at: 0,
            updated_at: 0,
        };
        assert!(r.is_followed());
        r.status = STATUS_UNFOLLOWED;
        assert!(!r.is_followed());
    }
}
