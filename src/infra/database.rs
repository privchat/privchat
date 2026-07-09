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

//! 数据库连接管理

use crate::config::DatabaseConfig;
use sqlx::{PgPool, postgres::PgPoolOptions};
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
        Self::new_with_config(database_url, &DatabaseConfig::default()).await
    }

    /// 使用显式配置创建新的数据库连接池。
    ///
    /// `statement_timeout_ms > 0` 时，会在每条连接建立后设置 PostgreSQL
    /// `statement_timeout`，给散落在 repository/service 层的 SQL 一个统一超时保护。
    pub async fn new_with_config(
        database_url: &str,
        config: &DatabaseConfig,
    ) -> Result<Self, sqlx::Error> {
        info!(
            "🔌 正在连接 PostgreSQL 数据库: {}, max_connections={}, min_connections={}, acquire_timeout={}s, statement_timeout={}ms",
            mask_database_url(database_url),
            config.max_connections,
            config.min_connections,
            config.acquire_timeout_seconds,
            config.statement_timeout_ms,
        );

        let mut options = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .min_connections(config.min_connections)
            .acquire_timeout(Duration::from_secs(config.acquire_timeout_seconds))
            .idle_timeout(Duration::from_secs(config.idle_timeout_seconds))
            .max_lifetime(Duration::from_secs(config.max_lifetime_seconds));

        if config.statement_timeout_ms > 0 {
            let statement_timeout_ms = config.statement_timeout_ms;
            options = options.after_connect(move |conn, _meta| {
                Box::pin(async move {
                    let timeout = format!("{}ms", statement_timeout_ms);
                    sqlx::query("SELECT set_config('statement_timeout', $1, false)")
                        .bind(timeout)
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            });
        }

        let pool = options.connect(database_url).await.map_err(|e| {
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
