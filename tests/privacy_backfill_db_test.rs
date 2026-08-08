// 隐私设置回填的真库门禁。
//
// 🔴 上一版这里只有一条「手动把 report.undecodable 加一然后断言它等于 1」的
// 假测试——它根本没调用 `backfill_from_entries`，所以也就发现不了
// 「DB 非空就整用户跳过」那个 P0：注释写着只补缺失字段，实现却把用户整个略过，
// Redis 里其余限制字段永久丢失。
//
// 这一组直接调被测函数、打真库，覆盖：空 DB 回填 / 部分合并 / 冲突时 DB 优先 /
// 非法数据不写 / dry-run 不写 / 重复执行幂等 / 用户不存在。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::service::privacy_backfill::backfill_from_entries;

const USER: i64 = 9_990_001;
const MISSING_USER: i64 = 9_990_099;

fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn pool() -> Option<Arc<sqlx::PgPool>> {
    let url = privchat::require_test_database_url()?;
    Some(Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    ))
}

/// Redis 里存的是**完整**序列化结构（`set_privacy_settings` 写的是整份），
/// 所以回填的输入也必须是完整形状。
fn redis_entry(user_id: i64, non_friend: bool, search_by_phone: bool) -> serde_json::Value {
    serde_json::json!({
        "user_id": user_id,
        "allow_add_by_group": true,
        "allow_add_by_card": true,
        "allow_search_by_phone": search_by_phone,
        "allow_search_by_username": true,
        "allow_search_by_email": true,
        "allow_search_by_qrcode": true,
        "allow_view_by_non_friend": true,
        "allow_receive_message_from_non_friend": non_friend,
        "created_at": "2026-01-01T00:00:00Z",
        "updated_at": "2026-01-01T00:00:00Z",
    })
}

async fn reset(pool: &sqlx::PgPool) {
    sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, display_name, qr_key)
        VALUES ($1, 'bf_user', 'bf_user', $2)
        ON CONFLICT (user_id) DO NOTHING
        "#,
    )
    .bind(USER)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool)
    .await
    .expect("ensure user");
    sqlx::query("UPDATE privchat_users SET privacy_settings = '{}' WHERE user_id = $1")
        .bind(USER)
        .execute(pool)
        .await
        .expect("reset settings");
    sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
        .bind(MISSING_USER)
        .execute(pool)
        .await
        .expect("ensure missing user absent");
}

async fn sync_version(pool: &sqlx::PgPool) -> i64 {
    let (version,): (i64,) =
        sqlx::query_as("SELECT sync_version FROM privchat_users WHERE user_id = $1")
            .bind(USER)
            .fetch_one(pool)
            .await
            .expect("read sync_version");
    version
}

async fn stored(pool: &sqlx::PgPool) -> serde_json::Value {
    let (value,): (serde_json::Value,) =
        sqlx::query_as("SELECT privacy_settings FROM privchat_users WHERE user_id = $1")
            .bind(USER)
            .fetch_one(pool)
            .await
            .expect("read settings");
    value
}

/// DB 为空 → Redis 的值整份落库。
#[tokio::test]
async fn an_empty_row_receives_the_redis_settings() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    let report = backfill_from_entries(&pool, vec![(USER as u64, redis_entry(USER, false, true))], false)
        .await
        .expect("backfill");
    assert_eq!((report.scanned, report.written, report.undecodable), (1, 1, 0));
    assert_eq!(
        stored(&pool).await.get("allow_receive_message_from_non_friend"),
        Some(&serde_json::Value::Bool(false)),
    );
}

/// 🔴 DB 已有**部分**字段：必须合并，DB 侧优先，Redis 补齐其余键。
///
/// 这正是上一版的 P0：DB 非空就整用户跳过，Redis 里其它限制字段永久丢失。
#[tokio::test]
async fn a_partially_populated_row_is_merged_with_the_database_winning() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    // 用户在新版本上改过一个字段：把「非好友消息」打开。
    sqlx::query(
        r#"UPDATE privchat_users
           SET privacy_settings = '{"allow_receive_message_from_non_friend": true}'
           WHERE user_id = $1"#,
    )
    .bind(USER)
    .execute(pool.as_ref())
    .await
    .expect("seed partial db settings");

    // Redis 里那份是旧的：非好友消息=false，而且还带着「不许手机号搜到我」。
    let report = backfill_from_entries(
        &pool,
        vec![(USER as u64, redis_entry(USER, false, false))],
        false,
    )
    .await
    .expect("backfill");
    assert_eq!(report.written, 1, "有部分字段的用户也必须处理，不能跳过");

    let after = stored(&pool).await;
    assert_eq!(
        after.get("allow_receive_message_from_non_friend"),
        Some(&serde_json::Value::Bool(true)),
        "DB 里已有的键优先：用户在新版本改的值不能被旧的 Redis 值盖掉",
    );
    assert_eq!(
        after.get("allow_search_by_phone"),
        Some(&serde_json::Value::Bool(false)),
        "Redis 独有的限制字段必须补进来——跳过整个用户就是把它弄丢",
    );
}

/// 解析不出来的条目：不写、计入审计。
#[tokio::test]
async fn an_undecodable_entry_is_audited_and_never_written() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    let report = backfill_from_entries(
        &pool,
        vec![(USER as u64, serde_json::json!({ "not": "settings" }))],
        false,
    )
    .await
    .expect("backfill");
    assert_eq!((report.written, report.undecodable), (0, 1));
    assert_eq!(
        stored(&pool).await,
        serde_json::json!({}),
        "解析不出来的条目一个字节都不该落库",
    );
}

/// dry-run 不写库。
#[tokio::test]
async fn a_dry_run_reports_without_writing() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    let report = backfill_from_entries(&pool, vec![(USER as u64, redis_entry(USER, false, true))], true)
        .await
        .expect("dry run");
    assert_eq!(report.written, 1, "dry-run 仍要报「会写多少」");
    assert_eq!(stored(&pool).await, serde_json::json!({}), "但一个字节都不写");
}

/// 重复执行幂等——包括**没有副作用**，不只是结果相同。
///
/// 🔴 只比 JSON 是不够的：`privchat_users` 上挂着 sync_version trigger，
/// 第二次照写一遍同样的值也会 bump 版本，把所有回填过的用户重新推进一轮增量同步。
/// 所以这里连 `sync_version` 一起断言。
#[tokio::test]
async fn running_twice_has_no_second_effect() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    let entry = redis_entry(USER, false, true);
    backfill_from_entries(&pool, vec![(USER as u64, entry.clone())], false)
        .await
        .expect("first");
    let first = stored(&pool).await;
    let first_version = sync_version(&pool).await;

    let report = backfill_from_entries(&pool, vec![(USER as u64, entry)], false)
        .await
        .expect("second");

    assert_eq!(stored(&pool).await, first, "重复跑结果必须一致");
    assert_eq!(
        sync_version(&pool).await,
        first_version,
        "第二次不能再 UPDATE：会 bump sync_version，把用户重新推进一轮增量同步",
    );
    assert_eq!(
        (report.written, report.unchanged),
        (0, 1),
        "第二次应报「未变更」，而不是又写了一遍",
    );
}

/// 用户已不存在：计入审计，不报成功。
#[tokio::test]
async fn an_entry_for_a_deleted_user_is_counted_not_written() {
    let _guard = fixture_lock().lock().await;
    let Some(pool) = pool().await else { return };
    reset(&pool).await;

    let report = backfill_from_entries(
        &pool,
        vec![(MISSING_USER as u64, redis_entry(MISSING_USER, false, true))],
        false,
    )
    .await
    .expect("backfill");
    assert_eq!((report.written, report.user_missing), (0, 1));
}
