//! 数据库连接管理

use sqlx::{postgres::PgPoolOptions, PgPool};
use std::time::Duration;
use tracing::{error, info};

/// 数据库连接池管理器
#[derive(Clone)]
pub struct Database {
    pool: PgPool,
}

impl Database {
    /// 创建新的数据库连接池
    ///
    /// 如果连接失败，会返回错误，调用方应该直接退出程序
    pub async fn new(database_url: &str) -> Result<Self, sqlx::Error> {
        info!(
            "🔌 正在连接 PostgreSQL 数据库: {}",
            mask_database_url(database_url)
        );

        let pool = PgPoolOptions::new()
            .max_connections(20)
            .min_connections(5)
            .acquire_timeout(Duration::from_secs(10))
            .idle_timeout(Duration::from_secs(600))
            .max_lifetime(Duration::from_secs(1800))
            .connect(database_url)
            .await
            .map_err(|e| {
                error!("错误详情: {}", e);
                e
            })?;

        // 测试连接
        sqlx::query("SELECT 1").execute(&pool).await.map_err(|e| {
            error!("错误详情: {}", e);
            e
        })?;

        info!("✅ PostgreSQL 数据库连接成功");

        Ok(Self { pool })
    }

    /// 获取连接池
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// 检查数据库连接
    pub async fn check_connection(&self) -> Result<(), sqlx::Error> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }
}

/// 隐藏数据库 URL 中的敏感信息（用于日志）
fn mask_database_url(url: &str) -> String {
    // 简单的 URL 掩码：隐藏密码部分
    // postgres://user:password@host:port/dbname -> postgres://user:***@host:port/dbname
    if let Some(at_pos) = url.find('@') {
        if let Some(scheme_end) = url.find("://") {
            let scheme = &url[..scheme_end + 3];
            let rest = &url[scheme_end + 3..];
            if let Some(colon_pos) = rest.find(':') {
                let user = &rest[..colon_pos];
                let after_at = &url[at_pos..];
                return format!("{}{}:***{}", scheme, user, after_at);
            }
        }
    }
    url.to_string()
}
