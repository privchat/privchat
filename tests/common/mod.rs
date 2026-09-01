//! 测试共用的一次性数据库。
//!
//! 🔴 清理必须走 `Drop`，不能靠测试跑到最后调一个 `cleanup()`。任何一条断言 panic
//! 都会跳过那个调用，留下数据库**和活着的连接**；下一轮 `DROP DATABASE` 会因为
//! "database is being accessed by other users" 失败，于是一次断言失败会连累后面每一轮。

#![allow(dead_code)]

use sqlx::{Connection, Executor, PgConnection};

/// 一个用完即毁的数据库。
pub struct ScratchDatabase {
    base: String,
    name: String,
}

impl ScratchDatabase {
    pub async fn create(name: &str) -> Option<Self> {
        let url = privchat::require_test_database_url()?;
        let (base, _) = url.rsplit_once('/').expect("url must carry a database name");
        // 🔴 库名必须带进程 id。固定名字在两个 CI shard（或本地同时跑两次）时会撞上，
        // 而 `Drop` 会 `pg_terminate_backend` 掉"同名"库的全部连接——一组测试因此能
        // 把另一组正在跑的测试踢断，表现成随机的连接错误。
        let db = Self {
            base: base.to_string(),
            name: format!("privchat_run_{}_{name}", std::process::id()),
        };
        db.recreate().await;
        Some(db)
    }

    async fn admin(&self) -> PgConnection {
        PgConnection::connect(&format!("{}/postgres", self.base))
            .await
            .expect("connect to postgres")
    }

    async fn recreate(&self) {
        let mut admin = self.admin().await;
        Self::force_drop(&mut admin, &self.name).await;
        admin
            .execute(format!("CREATE DATABASE {}", self.name).as_str())
            .await
            .expect("create database");
    }

    /// 先踢掉残留连接再删；否则一次 panic 就能让这个库永远删不掉。
    pub async fn force_drop(admin: &mut PgConnection, name: &str) {
        let _ = admin
            .execute(
                format!(
                    "SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
                     WHERE datname = '{name}' AND pid <> pg_backend_pid()"
                )
                .as_str(),
            )
            .await;
        let _ = admin
            .execute(format!("DROP DATABASE IF EXISTS {name}").as_str())
            .await;
    }

    /// 这个库的连接串。
    pub fn url(&self) -> String {
        format!("{}/{}", self.base, self.name)
    }

    pub async fn connect(&self) -> PgConnection {
        PgConnection::connect(&format!("{}/{}", self.base, self.name))
            .await
            .expect("connect")
    }
}

impl Drop for ScratchDatabase {
    fn drop(&mut self) {
        let base = self.base.clone();
        let name = self.name.clone();
        // Drop 里不能 await，另起一个 runtime 收尾。
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime");
            rt.block_on(async move {
                if let Ok(mut admin) = PgConnection::connect(&format!("{base}/postgres")).await {
                    ScratchDatabase::force_drop(&mut admin, &name).await;
                }
            });
        })
        .join()
        .ok();
    }
}


#[macro_export]
macro_rules! scratch_or_skip {
    ($name:expr) => {
        match $crate::common::ScratchDatabase::create($name).await {
            Some(db) => db,
            None => return,
        }
    };
}
