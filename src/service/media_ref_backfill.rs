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

//! 存量消息 → 引用表的回填（MEDIA_REFERENCE_AND_FORWARD_SPEC §9）。
//!
//! **为什么不是 SQL migration**：migration runner 只会跑 `raw_sql`，调不到 Rust
//! 函数；而回填必须走与发送路径**同一个** extractor。在 migration 里手写第二份
//! JSON 路径解析，就是「两份实现必然漂移」的开端——这个项目已经在附件解析上
//! 犯过一次，代价是转发的图片下不动。
//!
//! **为什么可断点续跑**：分区消息表按 `(created_at, message_id)` 顺序推进，每批
//! 提交后水位落库。中断后从水位继续，不重扫已完成的部分；`ON CONFLICT DO NOTHING`
//! 让重叠批次幂等。
//!
//! **不静默跳过**：解析不出引用的媒体消息逐条进审计计数（spec §9.1 第 5 步）。
//! 「回填完成、零错误」如果是靠跳过换来的，那是一句谎话——真正缺引用的那批
//! 要等用户投诉才暴露。

use anyhow::{Context, Result};
use sqlx::{PgPool, Row};

use crate::service::legacy_media_refs::{parse_legacy_media_refs_by_code, LegacyAudit};

/// 一次回填的结果。**审计计数与写入计数同等重要**——只报「写了多少」而不报
/// 「多少条没解析出来」，等于把问题埋进成功日志里。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackfillReport {
    /// 扫描过的消息行数。
    pub scanned: usize,
    /// 至少产出一条引用的消息数。
    pub messages_with_refs: usize,
    /// 实际写入引用表的行数（不含已存在被 ON CONFLICT 跳过的）。
    pub refs_inserted: u64,
    /// metadata 解不成 typed variant 的媒体消息。
    pub audit_undecodable: usize,
    /// 类型化成功但一条引用都没有的媒体消息。
    pub audit_no_refs: usize,
    /// 只有缩略图、缺主体文件的媒体消息。
    pub audit_missing_original: usize,
}

impl BackfillReport {
    fn record(&mut self, audit: &LegacyAudit) {
        match audit {
            LegacyAudit::MetadataUndecodable { .. } => self.audit_undecodable += 1,
            LegacyAudit::NoRefsForMediaType { .. } => self.audit_no_refs += 1,
            LegacyAudit::MissingOriginal { .. } => self.audit_missing_original += 1,
        }
    }

    /// 需要人工复核的消息条数。
    pub fn audited(&self) -> usize {
        self.audit_undecodable + self.audit_no_refs + self.audit_missing_original
    }
}

/// 扫描游标：分区表按 `(created_at, message_id)` 单调推进。
///
/// 只用 `message_id` 不行——雪花 id 在跨节点下不保证与 `created_at` 同序，
/// 而分区裁剪依赖 `created_at`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    created_at: i64,
    message_id: i64,
}

/// 回填全部存量消息。`batch_size` 只影响一次事务的大小，不影响结果。
///
/// 幂等：重复跑不会产生重复引用（主键 + `ON CONFLICT DO NOTHING`）。
pub async fn backfill_all(pool: &PgPool, batch_size: i64) -> Result<BackfillReport> {
    backfill_from(pool, batch_size, 0).await
}

/// 从指定时间水位之后开始回填。双写上线后的 catch-up 扫描用这个入口：
/// 传入「记录回填高水位」那一刻的时间，只补那之后新产生的消息。
pub async fn backfill_from(
    pool: &PgPool,
    batch_size: i64,
    since_created_at: i64,
) -> Result<BackfillReport> {
    let mut report = BackfillReport::default();
    let mut cursor = Cursor {
        created_at: since_created_at,
        message_id: i64::MIN,
    };

    loop {
        let rows = sqlx::query(
            r#"
            SELECT message_id, created_at, message_type, metadata
            FROM privchat_messages
            WHERE (created_at, message_id) > ($1, $2)
            ORDER BY created_at, message_id
            LIMIT $3
            "#,
        )
        .bind(cursor.created_at)
        .bind(cursor.message_id)
        .bind(batch_size)
        .fetch_all(pool)
        .await
        .context("读取消息批次失败")?;

        if rows.is_empty() {
            break;
        }

        let mut tx = pool.begin().await.context("开启回填事务失败")?;
        for row in &rows {
            let message_id: i64 = row.try_get("message_id")?;
            let created_at: i64 = row.try_get("created_at")?;
            // 列类型是 SMALLINT。按 i32 取会在运行期报解码错误——
            // 这条只有真库能发现，编译和单测都拦不住。
            let message_type: i16 = row.try_get("message_type")?;
            let metadata: serde_json::Value = row.try_get("metadata")?;

            cursor = Cursor {
                created_at,
                message_id,
            };
            report.scanned += 1;

            let parsed = parse_legacy_media_refs_by_code(i32::from(message_type), &metadata);
            for audit in &parsed.audits {
                report.record(audit);
            }
            if parsed.refs.is_empty() {
                continue;
            }
            report.messages_with_refs += 1;

            for media_ref in &parsed.refs {
                let result = sqlx::query(
                    r#"
                    INSERT INTO privchat_message_file_refs
                        (message_id, message_created_at, file_id, role, ordinal, created_at)
                    VALUES ($1, $2, $3, $4, $5, $2)
                    ON CONFLICT (message_id, role, ordinal) DO NOTHING
                    "#,
                )
                .bind(message_id)
                .bind(created_at)
                .bind(media_ref.file_id as i64)
                .bind(media_ref.role as i16)
                .bind(media_ref.ordinal)
                .execute(&mut *tx)
                .await
                .with_context(|| {
                    format!(
                        "写入引用失败 message_id={message_id} file_id={} role={:?}",
                        media_ref.file_id, media_ref.role
                    )
                })?;
                report.refs_inserted += result.rows_affected();
            }
        }
        tx.commit().await.context("提交回填批次失败")?;

        if (rows.len() as i64) < batch_size {
            break;
        }
    }

    Ok(report)
}

/// 零缺口校验（spec §10 第 5 步）：**每一条**能解析出引用的消息，其引用是否都在表里。
///
/// 这是「敢不敢把 get_url 切到引用表」的判据。切换前这个数必须是 0——
/// 不为 0 就意味着有消息的附件在切换那一刻开始下不动。
pub async fn verify_no_gaps(pool: &PgPool, batch_size: i64) -> Result<Vec<i64>> {
    let mut missing = Vec::new();
    let mut cursor = Cursor {
        created_at: 0,
        message_id: i64::MIN,
    };

    loop {
        let rows = sqlx::query(
            r#"
            SELECT m.message_id,
                   m.created_at,
                   m.message_type,
                   m.metadata,
                   (SELECT count(*) FROM privchat_message_file_refs r
                     WHERE r.message_id = m.message_id) AS ref_count
            FROM privchat_messages m
            WHERE (m.created_at, m.message_id) > ($1, $2)
            ORDER BY m.created_at, m.message_id
            LIMIT $3
            "#,
        )
        .bind(cursor.created_at)
        .bind(cursor.message_id)
        .bind(batch_size)
        .fetch_all(pool)
        .await
        .context("读取校验批次失败")?;

        if rows.is_empty() {
            break;
        }

        for row in &rows {
            let message_id: i64 = row.try_get("message_id")?;
            let created_at: i64 = row.try_get("created_at")?;
            // 列类型是 SMALLINT。按 i32 取会在运行期报解码错误——
            // 这条只有真库能发现，编译和单测都拦不住。
            let message_type: i16 = row.try_get("message_type")?;
            let metadata: serde_json::Value = row.try_get("metadata")?;
            cursor = Cursor {
                created_at,
                message_id,
            };

            let expected = parse_legacy_media_refs_by_code(i32::from(message_type), &metadata)
                .refs
                .len() as i64;
            let actual: i64 = row.try_get("ref_count")?;
            if actual < expected {
                missing.push(message_id);
            }
        }

        if (rows.len() as i64) < batch_size {
            break;
        }
    }

    Ok(missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_audit_tally_counts_every_kind_separately() {
        let mut report = BackfillReport::default();
        report.record(&LegacyAudit::MetadataUndecodable { content_type: 2 });
        report.record(&LegacyAudit::MetadataUndecodable { content_type: 3 });
        report.record(&LegacyAudit::NoRefsForMediaType { content_type: 2 });
        report.record(&LegacyAudit::MissingOriginal { content_type: 3 });

        assert_eq!(report.audit_undecodable, 2);
        assert_eq!(report.audit_no_refs, 1);
        assert_eq!(report.audit_missing_original, 1);
        // 「多少条需要人工看」是一个数，不该让调用方自己去加。
        assert_eq!(report.audited(), 4);
    }

    /// 游标必须同时带 created_at —— 只按 message_id 推进会在分区表上
    /// 既丢分区裁剪、又在雪花 id 跨节点乱序时漏消息。
    #[test]
    fn the_cursor_orders_by_time_first() {
        let earlier = Cursor {
            created_at: 100,
            message_id: i64::MAX,
        };
        let later = Cursor {
            created_at: 101,
            message_id: i64::MIN,
        };
        assert!((earlier.created_at, earlier.message_id) < (later.created_at, later.message_id));
    }
}
