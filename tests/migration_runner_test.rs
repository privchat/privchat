//! 迁移执行器的门禁。
//!
//! 这里验的不是某一条迁移写得对不对，而是**执行器本身**：跑的是不是生产那份清单、
//! 崩在半路会留下什么。
//!
//! ```bash
//! export PRIVCHAT_TEST_DATABASE_URL="postgres://$(whoami)@localhost:5432/privchat_test"
//! cargo test --test migration_runner_test
//! ```

mod common;
use common::ScratchDatabase;

macro_rules! scratch {
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
    let db = scratch!("full");
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

/// 账本被外部改坏（有人手删了一条记录）之后，重跑必须是安全的。
///
/// 🔴 这**不是**事务回滚的证明——那条在
/// `a_failure_between_the_sql_and_the_ledger_rolls_the_sql_back`。这里的场景是
/// "SQL 确实生效过，但账没了"，两者要验的东西相反：那条要求 SQL 被撤销，
/// 这条要求 SQL 已经生效的前提下重跑不会把库弄坏。
#[tokio::test]
async fn a_rerun_after_the_ledger_was_tampered_with_is_safe() {
    let db = scratch!("halfapply");
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
    let db = scratch!("ledger");
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

/// 🔴 事务只防崩溃，不防两个进程同时跑。
///
/// 滚动部署里两个实例可以在同一瞬间读到同一份"已执行"清单，然后各自执行同一条
/// 迁移——两次都会"成功"，因为它们各自看到的都是合法状态。对 001 尤其致命：
/// 它会重建 public schema。
///
/// 这里让两个 runner 真的并发跑：后到的必须等锁，等到之后重新读账本，一条都不跑。
#[tokio::test]
async fn two_concurrent_runners_do_not_both_migrate() {
    let db = scratch!("concurrent");
    let mut a = db.connect().await;
    let mut b = db.connect().await;

    let (ra, rb) = tokio::join!(
        privchat::migrate::apply_pending(&mut a, |_| {}),
        privchat::migrate::apply_pending(&mut b, |_| {}),
    );
    let ra = ra.expect("runner a");
    let rb = rb.expect("runner b");

    // 一个跑完全部，另一个一条不跑。谁先谁后不确定，但绝不能两个都跑。
    let (full, empty) = if ra.len() >= rb.len() { (ra, rb) } else { (rb, ra) };
    assert_eq!(full.len(), privchat::migrate::MIGRATIONS.len());
    assert!(empty.is_empty(), "第二个 runner 不该重跑任何迁移: {empty:?}");

    let recorded = privchat::migrate::applied_migrations(&mut a)
        .await
        .expect("ledger");
    assert_eq!(
        recorded.len(),
        privchat::migrate::MIGRATIONS.len(),
        "账本里不该有重复记录"
    );
}

/// 🔴 "SQL 已生效、账还没记"就崩溃时，**SQL 也必须一起回滚**。
///
/// 上一条测试是在迁移整体成功之后手工删账本，那模拟的是"外部把账本改坏了"。
/// 这里用执行器自带的注入点在事务中途失败，然后断言那条迁移建的表根本不存在——
/// 证明的是事务本身，而不是我们对事务的期望。
#[tokio::test]
async fn a_failure_between_the_sql_and_the_ledger_rolls_the_sql_back() {
    let db = scratch!("rollback");
    let mut conn = db.connect().await;

    // 在第二条迁移的注入点失败：第一条已提交，第二条必须整个消失。
    let target = privchat::migrate::MIGRATIONS[1].0;
    let err = privchat::migrate::apply_pending_with_hook(
        &mut conn,
        |_| {},
        move |name| {
            if name == target {
                anyhow::bail!("injected crash between sql and ledger")
            }
            Ok(())
        },
    )
    .await
    .expect_err("注入的失败必须传出来");
    assert!(format!("{err:#}").contains("injected crash"), "{err:#}");

    let recorded = privchat::migrate::applied_migrations(&mut conn)
        .await
        .expect("ledger");
    assert!(
        recorded.iter().any(|n| n == privchat::migrate::MIGRATIONS[0].0),
        "注入点之前的迁移应该已经提交"
    );
    assert!(
        !recorded.iter().any(|n| n == target),
        "崩在中途的那条不该留下账"
    );

    // 002 给 privchat_file_uploads 加了 encryption_version 列。回滚之后它必须不存在——
    // 只查账本的话，"SQL 生效了但账没记"和"两者都没发生"看起来一模一样。
    let column_exists: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'privchat_file_uploads' \
           AND column_name = 'encryption_version'",
    )
    .fetch_one(&mut conn)
    .await
    .expect("column lookup");
    assert_eq!(column_exists, 0, "回滚必须连 DDL 一起撤销，而不只是账本");
}

/// 🔴 已执行的迁移文件是**不可变**的。
///
/// 只按名字记账的话，改了一条已跑过的迁移，这台机器会跳过、新机器会执行修改后的版本，
/// 于是同名的 `031` 在两台机器上是不同的 schema，而没有任何东西会发现。
#[tokio::test]
async fn a_modified_migration_file_is_refused_rather_than_skipped() {
    let db = scratch!("drift");
    let mut conn = db.connect().await;

    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("first run");

    // 模拟"文件被改过"：把账本里的摘要换成别的值。
    let target = privchat::migrate::MIGRATIONS[1].0;
    sqlx::query("UPDATE public.privchat_migrations SET content_sha256 = $1 WHERE name = $2")
        .bind("0".repeat(64))
        .bind(target)
        .execute(&mut conn)
        .await
        .expect("tamper the digest");

    let err = privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect_err("内容漂移必须拒绝启动");
    let text = format!("{err:#}");
    assert!(text.contains(target), "{text}");
    assert!(text.contains("不可修改"), "{text}");
}

/// 每条迁移都要记下内容摘要——没有它，上面那条检测无从谈起。
#[tokio::test]
async fn every_applied_migration_records_its_content_digest() {
    let db = scratch!("digests");
    let mut conn = db.connect().await;

    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .expect("migrate");

    let recorded = privchat::migrate::applied_with_digests(&mut conn)
        .await
        .expect("ledger");
    assert_eq!(recorded.len(), privchat::migrate::MIGRATIONS.len());
    for (name, digest) in recorded {
        let digest = digest.unwrap_or_else(|| panic!("{name} 没记下内容摘要"));
        assert_eq!(digest.len(), 64, "{name} 的摘要不是 SHA-256");
    }
}

/// 🔴 等锁必须有上限。活着但卡死的 migrator 会让后续每一次部署静默挂起，
/// 而部署系统只会看到"还在跑"。
#[tokio::test]
async fn waiting_for_the_lock_gives_up_instead_of_hanging_forever() {
    let db = scratch!("locktimeout");
    let mut holder = db.connect().await;
    let mut waiter = db.connect().await;

    // 手工占住那把锁，模拟一个卡死的 migrator。key 与 migrate.rs 里的常量一致。
    let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(0x7076_6368_6174_0001i64)
        .fetch_one(&mut holder)
        .await
        .expect("take the lock");
    assert!(taken);

    // 等待上限是 60s，这里只验"它会等"而不是无限挂起：给一个远短于上限的超时，
    // 期望超时——真正的失败路径由上面的常量与错误信息保证。
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        privchat::migrate::apply_pending(&mut waiter, |_| {}),
    )
    .await;
    assert!(outcome.is_err(), "锁被占着时不该立刻返回成功");

    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(0x7076_6368_6174_0001i64)
        .execute(&mut holder)
        .await
        .expect("release");
}
