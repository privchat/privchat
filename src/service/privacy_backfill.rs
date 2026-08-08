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

//! 存量隐私设置回填：**Redis → DB**。
//!
//! 🔴 为什么需要这一步：`update_privacy_settings` 过去只写缓存，从不写
//! `privchat_users.privacy_settings`。所以线上此刻「用户改过的隐私设置」
//! 只存在于 Redis 里。直接部署「DB 为真源」的版本，那些设置会在 Redis
//! 过期或重启时**回落成默认值**——而默认是允许陌生人发消息。
//!
//! 也就是说：不先回填就上线，等于替一批用户把「仅接收好友消息」关掉。
//!
//! 用法：
//! ```bash
//! ./privchat --database-url "$DATABASE_URL" backfill-privacy-settings [--dry-run]
//! ```
//!
//! 幂等：DB 里已有的键**不被覆盖**（`patch || existing`，DB 侧优先），
//! 只补 Redis 有、DB 没有的部分。重复跑安全。

use anyhow::{Context, Result};
use sqlx::PgPool;

/// 一次回填的结论。审计数字与写入数字同等重要——只报「写了多少」
/// 而不报「多少条读不出来」，等于把问题埋进成功日志。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PrivacyBackfillReport {
    /// Redis 里扫到的隐私设置条数。
    pub scanned: usize,
    /// 实际写进 DB 的条数。
    pub written: usize,
    /// DB 里已经有值、因此跳过的条数。
    pub already_in_db: usize,
    /// Redis 里的值解析不出来的条数（不写，留待人工看）。
    pub undecodable: usize,
    /// 对应用户在 DB 里已不存在的条数。
    pub user_missing: usize,
}

/// 从 Redis 把隐私设置回填进 DB。
///
/// `entries` 是调用方从 Redis 扫出来的 `(user_id, 原始 JSON)`。把扫描抽在外面，
/// 是为了让这段逻辑可以脱离 Redis 测试——回填正确性不该只能在有 Redis 时验证。
pub async fn backfill_from_entries(
    pool: &PgPool,
    entries: Vec<(u64, serde_json::Value)>,
    dry_run: bool,
) -> Result<PrivacyBackfillReport> {
    let mut report = PrivacyBackfillReport {
        scanned: entries.len(),
        ..Default::default()
    };

    for (user_id, value) in entries {
        // 解不出来的不猜、不写，计入审计。
        if serde_json::from_value::<crate::model::privacy::UserPrivacySettings>(value.clone())
            .is_err()
        {
            report.undecodable += 1;
            continue;
        }

        let existing: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT privacy_settings FROM privchat_users WHERE user_id = $1")
                .bind(user_id as i64)
                .fetch_optional(pool)
                .await
                .context("查询现有隐私设置失败")?;

        let Some((existing,)) = existing else {
            report.user_missing += 1;
            continue;
        };
        if existing.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            // DB 已经有值：说明这个用户在新版本上改过，不要用 Redis 的旧值盖掉。
            report.already_in_db += 1;
            continue;
        }

        if dry_run {
            report.written += 1;
            continue;
        }

        sqlx::query(
            "UPDATE privchat_users \
             SET privacy_settings = $2::jsonb || COALESCE(privacy_settings, '{}'::jsonb) \
             WHERE user_id = $1",
        )
        .bind(user_id as i64)
        .bind(&value)
        .execute(pool)
        .await
        .context("回填隐私设置失败")?;
        report.written += 1;
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 解析不出来的条目不写、计入审计——不猜、也不静默跳过。
    #[test]
    fn undecodable_entries_are_audited_rather_than_guessed() {
        let mut report = PrivacyBackfillReport::default();
        report.undecodable += 1;
        assert_eq!(report.written, 0);
        assert_eq!(report.undecodable, 1);
    }
}
