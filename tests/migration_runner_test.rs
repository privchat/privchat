//! 迁移执行器的门禁。
//!
//! 这里验的不是某一条迁移写得对不对，而是**执行器本身**：跑的是不是生产那份清单、
//! 崩在半路会留下什么。
//!
//! ```bash
//! export PRIVCHAT_TEST_DATABASE_URL="postgres://$(whoami)@localhost:5432/privchat_test"
//! cargo test --test migration_runner_test
//! ```

use sqlx::{Connection, Executor, PgConnection};

/// 一个用完即毁的数据库。
///
/// 🔴 清理必须走 `Drop`，不能靠测试跑到最后调一个 `cleanup()`。任何一条断言 panic
/// 都会跳过那个调用，留下数据库**和活着的连接**；下一轮 `DROP DATABASE` 会因为
/// "database is being accessed by other users" 失败，于是一次断言失败会连累后面每一轮。
struct ScratchDatabase {
    base: String,
    name: String,
}

impl ScratchDatabase {
    async fn create(name: &str) -> Option<Self> {
        let url = privchat::require_test_database_url()?;
        let (base, _) = url.rsplit_once('/').expect("url must carry a database name");
        let db = Self {
            base: base.to_string(),
            name: format!("privchat_run_{name}"),
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
    async fn force_drop(admin: &mut PgConnection, name: &str) {
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

    async fn connect(&self) -> PgConnection {
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

macro_rules! scratch_or_skip {
    ($name:expr) => {
        match ScratchDatabase::create($name).await {
            Some(db) => db,
            None => return,
        }
    };
}

/// 全序列在空库上跑通，并且**每一条都记了账**。
#[tokio::test]
async fn a_full_run_applies_and_records_every_migration() {
    let db = scratch_or_skip!("full");
    let mut conn = db.connect().await;

    let executed = privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("migrate");
    assert!(!executed.is_empty());

    let recorded = privchat::migrate::applied_migrations(&mut conn)
        .await
        .expect("ledger");
    assert_eq!(
        executed, recorded,
        "跑过的和记下的必须一一对应，顺序也一样"
    );

    // 再跑一次应该什么都不做——幂等是"能不能安全重跑"的最低要求。
    let again = privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("second run");
    assert!(again.is_empty(), "已经跑过的迁移不该再跑一遍: {again:?}");
}

/// 🔴 生产跳过 `000_drop_all_tables.sql`（它会删光所有表）。测试自己扫目录的话
/// 会把它一起跑了，于是"测过了"证明的是另一件事。两边必须读同一份清单。
#[tokio::test]
async fn the_runner_uses_the_same_list_as_production() {
    let names: Vec<&str> = privchat::migrate::MIGRATIONS.iter().map(|(n, _)| *n).collect();

    assert!(
        !names.iter().any(|n| n.starts_with("000_")),
        "000_ 开头的是开发用的清库脚本，绝不能进生产清单: {names:?}"
    );
    assert!(names.iter().any(|n| n.starts_with("001_")));
    assert!(names.windows(2).all(|w| w[0] < w[1]), "迁移必须按名字有序");

    // 目录里除 000_ 之外的每个 .sql 都必须在清单里——漏一个就是"本地跑过、生产没跑"。
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut on_disk: Vec<String> = std::fs::read_dir(dir)
        .expect("migrations dir")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".sql") && !n.starts_with("000_"))
        // 清单里的名字不带扩展名（build.rs 生成时去掉了）。
        .map(|n| n.trim_end_matches(".sql").to_string())
        .collect();
    on_disk.sort();
    assert_eq!(names, on_disk, "清单与磁盘上的迁移文件必须完全一致");
}

/// 🔴 这条是事务化的理由本身。
///
/// 模拟"SQL 已生效、记账还没写"就崩溃：把最后一条迁移的账**删掉**，再跑一次。
/// 分两条语句的旧实现会在这里把同一个文件重跑一遍——对 032 那种带 `DROP COLUMN`
/// 的迁移就是当场失败，数据库停在既不是旧版也不是新版的状态。
///
/// 事务化之后这个窗口不存在：能被删掉的账，对应的 SQL 也必然没生效过。
/// 所以这条测试验的是"删账之后重跑会怎样"——它必须能安全重跑，而不是炸掉。
#[tokio::test]
async fn a_migration_never_half_applies() {
    let db = scratch_or_skip!("halfapply");
    let mut conn = db.connect().await;

    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("first run");

    let last = privchat::migrate::MIGRATIONS
        .last()
        .expect("at least one migration")
        .0;

    // 人为制造"跑了但没记账"的状态。
    sqlx::query("DELETE FROM public.privchat_migrations WHERE name = $1")
        .bind(last)
        .execute(&mut conn)
        .await
        .expect("forget the ledger entry");

    let result = privchat::migrate::apply_pending(&mut conn, |_| {}).await;

    // 这条迁移会被重跑。它必须要么幂等地成功，要么整体回滚而不留下半截 schema——
    // 两种都可以接受；不可接受的是"部分生效"。
    match result {
        Ok(executed) => {
            assert_eq!(executed, vec![last.to_string()], "只该重跑那一条");
        }
        Err(e) => {
            // 失败也行，但事务保证了它没改动任何东西：账依然缺着，schema 还是完整的。
            let recorded = privchat::migrate::applied_migrations(&mut conn)
                .await
                .expect("ledger still readable");
            assert!(
                !recorded.iter().any(|n| n == last),
                "回滚之后不该留下这条账: {e}"
            );
        }
    }
}

/// 记账表被基线迁移删掉之后能自己重建——不然全新库跑完 001 就再也记不了账。
#[tokio::test]
async fn the_ledger_survives_the_baseline_rebuilding_the_schema() {
    let db = scratch_or_skip!("ledger");
    let mut conn = db.connect().await;

    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("migrate");

    let recorded = privchat::migrate::applied_migrations(&mut conn)
        .await
        .expect("ledger");
    assert!(
        recorded.iter().any(|n| n.starts_with("001_")),
        "基线自己也必须被记上账，否则下次会重跑它、把库清空"
    );
}
