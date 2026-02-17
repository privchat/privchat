use anyhow::Result;
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// 登录日志记录
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct LoginLog {
    pub log_id: i64,
    pub user_id: i64,
    pub device_id: Uuid,

    // Token 信息
    pub token_jti: String,
    pub token_created_at: i64,
    pub token_first_used_at: i64,

    // 设备信息
    pub device_type: String,
    pub device_name: Option<String>,
    pub device_model: Option<String>,
    pub os_version: Option<String>,
    pub app_id: String,
    pub app_version: Option<String>,

    // 网络信息
    pub ip_address: String,
    pub user_agent: Option<String>,

    // 登录方式
    pub login_method: String,
    pub auth_source: Option<String>,

    // 安全信息
    pub status: i16, // 0: Success, 1: Suspicious, 2: Blocked
    pub risk_score: i16,
    pub risk_factors: Option<serde_json::Value>, // JSONB
    pub is_new_device: bool,
    pub is_new_location: bool,

    // 通知状态
    pub notification_sent: bool,
    pub notification_method: Option<String>,
    pub notification_sent_at: Option<i64>,

    // 其他
    pub metadata: Option<serde_json::Value>, // JSONB
    pub created_at: i64,
}

impl LoginLog {
    /// 是否为可疑登录
    pub fn is_suspicious(&self) -> bool {
        self.status == 1 || self.risk_score > 70
    }
}

/// 创建登录日志的请求（简化版）
#[derive(Debug, Clone)]
pub struct CreateLoginLogRequest {
    pub user_id: i64,
    pub device_id: Uuid,
    pub token_jti: String,
    pub token_created_at: i64,
    pub device_type: String,
    pub device_name: Option<String>,
    pub device_model: Option<String>,
    pub os_version: Option<String>,
    pub app_id: String,
    pub app_version: Option<String>,
    pub ip_address: String,
    pub user_agent: Option<String>,
    pub login_method: String,
    pub auth_source: Option<String>,
    pub status: i16, // 0: Success, 1: Suspicious, 2: Blocked
    pub risk_score: i16,
    pub is_new_device: bool,
    pub is_new_location: bool,
    pub risk_factors: Option<Vec<String>>,
    pub metadata: Option<serde_json::Value>,
}

/// 登录日志查询条件
#[derive(Debug, Clone, Default)]
pub struct LoginLogQuery {
    pub user_id: Option<i64>,
    pub device_id: Option<Uuid>,
    pub ip_address: Option<String>,
    pub status: Option<i16>, // 0: Success, 1: Suspicious, 2: Blocked
    pub start_time: Option<i64>,
    pub end_time: Option<i64>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// 登录日志仓库
pub struct LoginLogRepository {
    db_pool: Arc<PgPool>,
    /// token_jti → true 的本地缓存（L1-only，TTL 30s）
    token_logged_cache: Cache<String, bool>,
}

impl LoginLogRepository {
    pub fn new(db_pool: Arc<PgPool>) -> Self {
        let token_logged_cache = Cache::builder()
            .max_capacity(10_000)
            .time_to_live(Duration::from_secs(30))
            .build();
        Self {
            db_pool,
            token_logged_cache,
        }
    }

    /// 获取数据库连接池（用于管理 API）
    pub fn pool(&self) -> &PgPool {
        &self.db_pool
    }

    /// 创建登录日志
    pub async fn create(&self, req: CreateLoginLogRequest) -> Result<LoginLog> {
        let risk_factors_json = req
            .risk_factors
            .as_ref()
            .map(|factors| serde_json::to_value(factors).ok())
            .flatten();

        let record = sqlx::query_as!(
            LoginLog,
            r#"
            INSERT INTO privchat_login_logs (
                user_id, device_id, token_jti, token_created_at,
                device_type, device_name, device_model, os_version,
                app_id, app_version, ip_address, user_agent,
                login_method, auth_source,
                status, risk_score, is_new_device, is_new_location, risk_factors, metadata
            ) VALUES (
                $1, $2, $3, $4,
                $5, $6, $7, $8,
                $9, $10, $11, $12,
                $13, $14,
                $15, $16, $17, $18, $19, $20
            )
            RETURNING
                log_id, user_id, device_id, token_jti, token_created_at, token_first_used_at,
                device_type, device_name, device_model, os_version,
                app_id, app_version, ip_address, user_agent,
                login_method, auth_source,
                status, risk_score, is_new_device, is_new_location, risk_factors,
                notification_sent, notification_method, notification_sent_at,
                metadata, created_at
            "#,
            req.user_id,
            req.device_id,
            req.token_jti,
            req.token_created_at,
            req.device_type,
            req.device_name,
            req.device_model,
            req.os_version,
            req.app_id,
            req.app_version,
            req.ip_address,
            req.user_agent,
            req.login_method,
            req.auth_source,
            req.status,
            req.risk_score,
            req.is_new_device,
            req.is_new_location,
            risk_factors_json,
            req.metadata,
        )
        .fetch_one(&*self.db_pool)
        .await?;

        // 写入缓存，后续 is_token_logged 直接命中
        self.token_logged_cache
            .insert(record.token_jti.clone(), true)
            .await;

        Ok(record)
    }

    /// 检查 token 是否已被记录（防止重复记录）
    /// 带本地缓存（L1-only, TTL 30s），减少 DB 查询
    pub async fn is_token_logged(&self, token_jti: &str) -> Result<bool> {
        // L1 缓存命中
        if self.token_logged_cache.get(token_jti).await.is_some() {
            return Ok(true);
        }

        // 缓存 miss，查 DB
        let result = sqlx::query!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM privchat_login_logs
                WHERE token_jti = $1
            ) as "exists!"
            "#,
            token_jti
        )
        .fetch_one(&*self.db_pool)
        .await?;

        // DB 命中则回填缓存
        if result.exists {
            self.token_logged_cache
                .insert(token_jti.to_string(), true)
                .await;
        }

        Ok(result.exists)
    }

    /// 获取用户的登录日志
    pub async fn get_user_logs(&self, query: LoginLogQuery) -> Result<Vec<LoginLog>> {
        let mut sql = String::from(
            r#"
            SELECT
                log_id, user_id, device_id, token_jti, token_created_at, token_first_used_at,
                device_type, device_name, device_model, os_version,
                app_id, app_version, ip_address, user_agent,
                login_method, auth_source,
                status, risk_score, is_new_device, is_new_location, risk_factors,
                notification_sent, notification_method, notification_sent_at,
                metadata, created_at
            FROM privchat_login_logs
            WHERE 1=1
            "#,
        );

        let mut bind_count = 0;
        if query.user_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND user_id = ${}", bind_count));
        }
        if query.device_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND device_id = ${}", bind_count));
        }
        if query.ip_address.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND ip_address = ${}", bind_count));
        }
        if query.status.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND status = ${}", bind_count));
        }
        if query.start_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if query.end_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }

        sql.push_str(" ORDER BY created_at DESC");

        if let Some(_limit) = query.limit {
            bind_count += 1;
            sql.push_str(&format!(" LIMIT ${}", bind_count));
        }
        if let Some(_offset) = query.offset {
            bind_count += 1;
            sql.push_str(&format!(" OFFSET ${}", bind_count));
        }

        let mut query_builder = sqlx::query_as::<_, LoginLog>(&sql);

        if let Some(user_id) = query.user_id {
            query_builder = query_builder.bind(user_id);
        }
        if let Some(device_id) = query.device_id {
            query_builder = query_builder.bind(device_id);
        }
        if let Some(ip_address) = query.ip_address {
            query_builder = query_builder.bind(ip_address);
        }
        if let Some(status) = query.status {
            query_builder = query_builder.bind(status);
        }
        if let Some(start_time) = query.start_time {
            query_builder = query_builder.bind(start_time);
        }
        if let Some(end_time) = query.end_time {
            query_builder = query_builder.bind(end_time);
        }
        if let Some(limit) = query.limit {
            query_builder = query_builder.bind(limit);
        }
        if let Some(offset) = query.offset {
            query_builder = query_builder.bind(offset);
        }

        let logs = query_builder.fetch_all(&*self.db_pool).await?;

        Ok(logs)
    }

    /// 根据 log_id 获取登录日志详情（管理 API）
    pub async fn get_by_id(&self, log_id: i64) -> Result<Option<LoginLog>> {
        let log = sqlx::query_as!(
            LoginLog,
            r#"
            SELECT
                log_id, user_id, device_id, token_jti, token_created_at, token_first_used_at,
                device_type, device_name, device_model, os_version,
                app_id, app_version, ip_address, user_agent,
                login_method, auth_source,
                status, risk_score, is_new_device, is_new_location, risk_factors,
                notification_sent, notification_method, notification_sent_at,
                metadata, created_at
            FROM privchat_login_logs
            WHERE log_id = $1
            LIMIT 1
            "#,
            log_id,
        )
        .fetch_optional(&*self.db_pool)
        .await?;

        Ok(log)
    }

    /// 标记通知已发送
    pub async fn mark_notification_sent(
        &self,
        log_id: i64,
        method: &str,
        sent_at: i64,
    ) -> Result<()> {
        sqlx::query!(
            r#"
            UPDATE privchat_login_logs
            SET notification_sent = true,
                notification_method = $1,
                notification_sent_at = $2
            WHERE log_id = $3
            "#,
            method,
            sent_at,
            log_id
        )
        .execute(&*self.db_pool)
        .await?;

        Ok(())
    }

    /// 获取最近的登录 IP（用于判断是否为新 IP）
    pub async fn get_recent_login_ips(&self, user_id: i64, days: i32) -> Result<Vec<String>> {
        let since = chrono::Utc::now().timestamp_millis() - (days as i64 * 24 * 3600 * 1000);

        let records = sqlx::query!(
            r#"
            SELECT DISTINCT ip_address
            FROM privchat_login_logs
            WHERE user_id = $1
              AND created_at >= $2
            ORDER BY ip_address
            "#,
            user_id,
            since
        )
        .fetch_all(&*self.db_pool)
        .await?;

        Ok(records.into_iter().map(|r| r.ip_address).collect())
    }

    /// 获取可疑登录（用于安全监控）
    pub async fn get_suspicious_logins(&self, limit: i64) -> Result<Vec<LoginLog>> {
        let logs = sqlx::query_as!(
            LoginLog,
            r#"
            SELECT
                log_id, user_id, device_id, token_jti, token_created_at, token_first_used_at,
                device_type, device_name, device_model, os_version,
                app_id, app_version, ip_address, user_agent,
                login_method, auth_source,
                status, risk_score, is_new_device, is_new_location, risk_factors,
                notification_sent, notification_method, notification_sent_at,
                metadata, created_at
            FROM privchat_login_logs
            WHERE status = 1 OR risk_score > 70
            ORDER BY created_at DESC
            LIMIT $1
            "#,
            limit
        )
        .fetch_all(&*self.db_pool)
        .await?;

        Ok(logs)
    }
}
