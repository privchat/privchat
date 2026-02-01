//! 用户设置 Repository（ENTITY_SYNC_V1 user_settings，表为主）

use sqlx::PgPool;
use crate::error::{Result, ServerError};

/// 用户设置 Repository（表 privchat_user_settings）
pub struct UserSettingsRepository {
    pool: PgPool,
}

impl UserSettingsRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 获取 user_id 下 version > since_version 的项，按 version 排序，limit 条
    /// 返回 (items: (setting_key, value_json, version), next_version, has_more)
    pub async fn get_since(
        &self,
        user_id: u64,
        since_version: u64,
        limit: u32,
    ) -> Result<(Vec<(String, serde_json::Value, u64)>, u64, bool)> {
        #[derive(sqlx::FromRow)]
        struct Row {
            setting_key: String,
            value_json: serde_json::Value,
            version: i64,
        }
        let limit = limit.min(200).max(1) as i64;
        let rows = sqlx::query_as::<_, Row>(
            r#"
            SELECT setting_key, value_json, version
            FROM privchat_user_settings
            WHERE user_id = $1 AND version > $2
            ORDER BY version
            LIMIT $3
            "#
        )
        .bind(user_id as i64)
        .bind(since_version as i64)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("get_user_settings_since: {}", e)))?;

        let list: Vec<(String, serde_json::Value, u64)> = rows
            .into_iter()
            .map(|r| (r.setting_key, r.value_json, r.version as u64))
            .collect();
        let next_version = list.last().map(|(_, _, v)| *v).unwrap_or(since_version);
        let has_more = list.len() >= limit as usize;
        Ok((list, next_version, has_more))
    }

    /// 设置单条，返回新 version（该 user 下全局递增）
    pub async fn set(
        &self,
        user_id: u64,
        setting_key: &str,
        value: &serde_json::Value,
    ) -> Result<u64> {
        let next_v = self.next_version(user_id).await?;
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO privchat_user_settings (user_id, setting_key, value_json, version, updated_at)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (user_id, setting_key)
            DO UPDATE SET value_json = $3, version = $4, updated_at = $5
            "#
        )
        .bind(user_id as i64)
        .bind(setting_key)
        .bind(value)
        .bind(next_v as i64)
        .bind(now)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("set_user_setting: {}", e)))?;
        Ok(next_v)
    }

    /// 批量设置，共用一个新 version
    pub async fn set_batch(
        &self,
        user_id: u64,
        settings: &std::collections::HashMap<String, serde_json::Value>,
    ) -> Result<u64> {
        if settings.is_empty() {
            return Ok(self.current_max_version(user_id).await?.unwrap_or(0));
        }
        let next_v = self.next_version(user_id).await?;
        let now = chrono::Utc::now().timestamp_millis();
        for (k, v) in settings {
            sqlx::query(
                r#"
                INSERT INTO privchat_user_settings (user_id, setting_key, value_json, version, updated_at)
                VALUES ($1, $2, $3, $4, $5)
                ON CONFLICT (user_id, setting_key)
                DO UPDATE SET value_json = $3, version = $4, updated_at = $5
                "#
            )
            .bind(user_id as i64)
            .bind(k.as_str())
            .bind(v)
            .bind(next_v as i64)
            .bind(now)
            .execute(&self.pool)
            .await
            .map_err(|e| ServerError::Database(format!("set_user_settings_batch: {}", e)))?;
        }
        Ok(next_v)
    }

    async fn current_max_version(&self, user_id: u64) -> Result<Option<u64>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT COALESCE(MAX(version), 0)::BIGINT FROM privchat_user_settings WHERE user_id = $1"
        )
        .bind(user_id as i64)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("current_max_version: {}", e)))?;
        Ok(row.map(|r| r.0 as u64))
    }

    async fn next_version(&self, user_id: u64) -> Result<u64> {
        let v = self.current_max_version(user_id).await?.unwrap_or(0);
        Ok(v.saturating_add(1))
    }
}
