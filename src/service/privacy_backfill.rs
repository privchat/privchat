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
//! ./privchat --database-url "$DATABASE_URL" backfill-privacy-settings --input privacy.jsonl [--dry-run]
//! ```
//!
//! 幂等：DB 里已有的键**不被覆盖**（`patch || existing`，DB 侧优先），
//! 只补 Redis 有、DB 没有的部分。重复跑**不产生副作用**——值没变就不 UPDATE，
//! 因此不会触发 `privchat_users` 的 sync_version trigger 把用户重新推进增量同步。

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
    /// Redis 里的值解析不出来的条数（不写，留待人工看）。
    pub undecodable: usize,
    /// 对应用户在 DB 里已不存在的条数。
    pub user_missing: usize,
    /// DB 里已经是目标值、这次一个字节都没改的条数。
    ///
    /// 单列出来是因为它和 `written` 的运维含义不同：重跑一次全是 `unchanged`
    /// 才说明回填真的收敛了，而不只是又写了一遍同样的值。
    pub unchanged: usize,
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

        if dry_run {
            // dry-run 也要确认用户存在，否则「预演全绿、真跑一半用户不存在」。
            let exists: Option<(i64,)> =
                sqlx::query_as("SELECT user_id FROM privchat_users WHERE user_id = $1")
                    .bind(user_id as i64)
                    .fetch_optional(pool)
                    .await
                    .context("dry-run 查询用户失败")?;
            match exists {
                Some(_) => report.written += 1,
                None => report.user_missing += 1,
            }
            continue;
        }

        // 🔴 **永远执行 `redis || db`**，不能因为 DB 非空就整个用户跳过。
        //
        // DB 侧用的是部分字段 patch：用户在新版本上改过**一个**字段，
        // `privacy_settings` 就非空了。此时跳过，Redis 里那些**其它**限制字段
        // 就永久丢失——上一版注释写着「只补缺失字段」，实现却是整用户跳过，
        // 声明与实现相反。
        //
        // `$2 || COALESCE(privacy_settings,'{}')` 的顺序让 **DB 已有键优先**
        // （用户在新版本改的不被旧值盖掉），同时补齐 DB 缺的键。
        // 🔴 `WHERE ... IS DISTINCT FROM` 让重复执行**没有副作用**，而不只是结果相同。
        //
        // `privchat_users` 上挂着 sync_version trigger：只要 UPDATE 命中行就会 bump 版本，
        // 于是「再跑一遍确认一下」会把所有回填过的用户重新推进一轮增量同步。
        // 幂等的判据是不产生新的可观察效果，不是「JSON 比对下来一样」。
        //
        // 代价是 rows_affected=0 不再等于「用户不存在」，所以用 RETURNING 区分：
        // 有行返回 = 真写了；无行返回再查一次存在性。
        let written: Option<(i64,)> = sqlx::query_as(
            "UPDATE privchat_users \
             SET privacy_settings = $2::jsonb || COALESCE(privacy_settings, '{}'::jsonb) \
             WHERE user_id = $1 \
               AND privacy_settings IS DISTINCT FROM \
                   ($2::jsonb || COALESCE(privacy_settings, '{}'::jsonb)) \
             RETURNING user_id",
        )
        .bind(user_id as i64)
        .bind(&value)
        .fetch_optional(pool)
        .await
        .context("回填隐私设置失败")?;

        if written.is_some() {
            report.written += 1;
            continue;
        }

        // 没写：要么本来就已是目标值（幂等命中），要么用户不存在。
        let exists: Option<(i64,)> =
            sqlx::query_as("SELECT user_id FROM privchat_users WHERE user_id = $1")
                .bind(user_id as i64)
                .fetch_optional(pool)
                .await
                .context("查询用户是否存在失败")?;
        match exists {
            Some(_) => report.unchanged += 1,
            None => report.user_missing += 1,
        }
    }

    Ok(report)
}
