use privchat::rpc::message::status::policy::{
    ensure_read_list_allowed, ensure_read_stats_allowed, ReadReceiptMode,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

#[derive(Debug, Clone, Copy)]
struct UpsertCursorResult {
    last_read_pts: u64,
    advanced: bool,
}

async fn open_test_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .ok()
}

async fn cleanup(pool: &sqlx::PgPool, user_id: i64, channel_id: i64) {
    let _ = sqlx::query(
        "DELETE FROM privchat_channel_read_cursor WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(user_id)
    .bind(channel_id)
    .execute(pool)
    .await;
}

async fn upsert_cursor(
    pool: &sqlx::PgPool,
    user_id: i64,
    channel_id: i64,
    last_read_pts: i64,
) -> UpsertCursorResult {
    let row = sqlx::query(
        r#"
        WITH existing AS (
            SELECT last_read_pts
            FROM privchat_channel_read_cursor
            WHERE user_id = $1 AND channel_id = $2
        ),
        upserted AS (
            INSERT INTO privchat_channel_read_cursor (
                user_id, channel_id, last_read_pts, last_read_message_id, updated_at
            )
            VALUES ($1, $2, $3, NULL, NOW())
            ON CONFLICT (user_id, channel_id)
            DO UPDATE SET
                last_read_pts = GREATEST(privchat_channel_read_cursor.last_read_pts, EXCLUDED.last_read_pts),
                last_read_message_id = CASE
                    WHEN EXCLUDED.last_read_pts >= privchat_channel_read_cursor.last_read_pts
                    THEN EXCLUDED.last_read_message_id
                    ELSE privchat_channel_read_cursor.last_read_message_id
                END,
                updated_at = CASE
                    WHEN EXCLUDED.last_read_pts > privchat_channel_read_cursor.last_read_pts
                    THEN NOW()
                    ELSE privchat_channel_read_cursor.updated_at
                END
            RETURNING last_read_pts
        )
        SELECT
            upserted.last_read_pts,
            (
                (SELECT last_read_pts FROM existing LIMIT 1) IS NULL
                OR $3 > (SELECT last_read_pts FROM existing LIMIT 1)
            ) AS advanced
        FROM upserted
        "#,
    )
    .bind(user_id)
    .bind(channel_id)
    .bind(last_read_pts)
    .fetch_one(pool)
    .await
    .expect("upsert cursor");

    UpsertCursorResult {
        last_read_pts: row.get::<i64, _>("last_read_pts") as u64,
        advanced: row.get::<bool, _>("advanced"),
    }
}

async fn count_read_members_projection(
    pool: &sqlx::PgPool,
    channel_id: i64,
    message_pts: i64,
    member_ids: &[i64],
) -> i64 {
    let row = sqlx::query(
        r#"
        SELECT COUNT(*)::BIGINT AS read_count
        FROM privchat_channel_read_cursor
        WHERE channel_id = $1
          AND user_id = ANY($2)
          AND last_read_pts >= $3
        "#,
    )
    .bind(channel_id)
    .bind(member_ids)
    .bind(message_pts)
    .fetch_one(pool)
    .await
    .expect("count projection");
    row.get::<i64, _>("read_count")
}

async fn list_read_members_projection(
    pool: &sqlx::PgPool,
    channel_id: i64,
    message_pts: i64,
    member_ids: &[i64],
) -> Vec<(i64, i64)> {
    let rows = sqlx::query(
        r#"
        SELECT user_id, last_read_pts
        FROM privchat_channel_read_cursor
        WHERE channel_id = $1
          AND user_id = ANY($2)
          AND last_read_pts >= $3
        ORDER BY updated_at DESC, user_id DESC
        "#,
    )
    .bind(channel_id)
    .bind(member_ids)
    .bind(message_pts)
    .fetch_all(pool)
    .await
    .expect("list projection");
    rows.into_iter()
        .map(|r| (r.get::<i64, _>("user_id"), r.get::<i64, _>("last_read_pts")))
        .collect()
}

#[tokio::test]
async fn upsert_max_is_monotonic() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip upsert_max_is_monotonic: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_900_101_i64;
    let channel_id = 9_800_101_i64;
    cleanup(&pool, user_id, channel_id).await;

    let _ = upsert_cursor(&pool, user_id, channel_id, 100).await;
    let _ = upsert_cursor(&pool, user_id, channel_id, 120).await;
    let _ = upsert_cursor(&pool, user_id, channel_id, 110).await;

    let row = sqlx::query(
        "SELECT last_read_pts FROM privchat_channel_read_cursor WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_one(&pool)
    .await
    .expect("query final cursor");
    assert_eq!(row.get::<i64, _>("last_read_pts"), 120);
    cleanup(&pool, user_id, channel_id).await;
}

#[tokio::test]
async fn upsert_max_is_idempotent() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip upsert_max_is_idempotent: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_900_102_i64;
    let channel_id = 9_800_102_i64;
    cleanup(&pool, user_id, channel_id).await;

    let first = upsert_cursor(&pool, user_id, channel_id, 200).await;
    let second = upsert_cursor(&pool, user_id, channel_id, 200).await;
    assert!(first.advanced);
    assert!(!second.advanced);
    assert_eq!(second.last_read_pts, 200);

    cleanup(&pool, user_id, channel_id).await;
}

#[tokio::test]
async fn concurrent_mark_read_keeps_max_pts() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip concurrent_mark_read_keeps_max_pts: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_900_103_i64;
    let channel_id = 9_800_103_i64;
    cleanup(&pool, user_id, channel_id).await;

    let p1 = pool.clone();
    let p2 = pool.clone();
    let f1 = tokio::spawn(async move {
        upsert_cursor(&p1, user_id, channel_id, 150).await;
    });
    let f2 = tokio::spawn(async move {
        upsert_cursor(&p2, user_id, channel_id, 180).await;
    });
    let _ = tokio::join!(f1, f2);

    let row = sqlx::query(
        "SELECT last_read_pts FROM privchat_channel_read_cursor WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_one(&pool)
    .await
    .expect("query final cursor");
    assert_eq!(row.get::<i64, _>("last_read_pts"), 180);
    cleanup(&pool, user_id, channel_id).await;
}

#[tokio::test]
async fn restart_preserves_channel_read_cursor() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip restart_preserves_channel_read_cursor: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_900_104_i64;
    let channel_id = 9_800_104_i64;
    cleanup(&pool, user_id, channel_id).await;

    let _ = upsert_cursor(&pool, user_id, channel_id, 260).await;
    pool.close().await;

    let Some(pool2) = open_test_pool().await else {
        panic!("reconnect test db failed");
    };
    let row = sqlx::query(
        "SELECT last_read_pts FROM privchat_channel_read_cursor WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_one(&pool2)
    .await
    .expect("query after reconnect");
    assert_eq!(row.get::<i64, _>("last_read_pts"), 260);
    cleanup(&pool2, user_id, channel_id).await;
}

#[tokio::test]
async fn older_cursor_update_does_not_rollback() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip older_cursor_update_does_not_rollback: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_900_105_i64;
    let channel_id = 9_800_105_i64;
    cleanup(&pool, user_id, channel_id).await;

    let _ = upsert_cursor(&pool, user_id, channel_id, 300).await;
    let stale = upsert_cursor(&pool, user_id, channel_id, 180).await;
    assert!(!stale.advanced);
    assert_eq!(stale.last_read_pts, 300);

    let row = sqlx::query(
        "SELECT last_read_pts FROM privchat_channel_read_cursor WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(user_id)
    .bind(channel_id)
    .fetch_one(&pool)
    .await
    .expect("query final cursor after stale update");
    assert_eq!(row.get::<i64, _>("last_read_pts"), 300);
    cleanup(&pool, user_id, channel_id).await;
}

#[tokio::test]
async fn read_stats_projects_from_cursor() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip read_stats_projects_from_cursor: DATABASE_URL not configured");
        return;
    };
    let channel_id = 9_800_201_i64;
    let members = [9_900_201_i64, 9_900_202_i64, 9_900_203_i64, 9_900_204_i64];
    // 非成员，但 cursor 足够大，不应计入。
    let non_member = 9_900_299_i64;
    let message_pts = 120_i64;

    for uid in members.iter().chain(std::iter::once(&non_member)) {
        cleanup(&pool, *uid, channel_id).await;
    }

    // < P
    let _ = upsert_cursor(&pool, members[0], channel_id, 119).await;
    // = P
    let _ = upsert_cursor(&pool, members[1], channel_id, 120).await;
    // > P
    let _ = upsert_cursor(&pool, members[2], channel_id, 180).await;
    // members[3] 无 cursor 记录
    // 非成员应被 member_ids 过滤
    let _ = upsert_cursor(&pool, non_member, channel_id, 999).await;

    let read_count = count_read_members_projection(&pool, channel_id, message_pts, &members).await;
    assert_eq!(read_count, 2, "only =P and >P members should be counted");

    for uid in members.iter().chain(std::iter::once(&non_member)) {
        cleanup(&pool, *uid, channel_id).await;
    }
}

#[tokio::test]
async fn read_list_projects_from_cursor() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip read_list_projects_from_cursor: DATABASE_URL not configured");
        return;
    };
    let channel_id = 9_800_202_i64;
    let members = [9_900_211_i64, 9_900_212_i64, 9_900_213_i64, 9_900_214_i64];
    let non_member = 9_900_299_i64;
    let message_pts = 500_i64;

    for uid in members.iter().chain(std::iter::once(&non_member)) {
        cleanup(&pool, *uid, channel_id).await;
    }

    // 仅 members[1], members[2] 满足 >= P
    let _ = upsert_cursor(&pool, members[0], channel_id, 499).await; // < P
    let _ = upsert_cursor(&pool, members[1], channel_id, 500).await; // = P
    let _ = upsert_cursor(&pool, members[2], channel_id, 520).await; // > P
                                                                     // members[3] 无 cursor
    let _ = upsert_cursor(&pool, non_member, channel_id, 900).await; // 非成员

    // 让排序稳定：members[1] 更新得更晚，应排前。
    let _ = sqlx::query(
        "UPDATE privchat_channel_read_cursor SET updated_at = NOW() - INTERVAL '30 seconds' WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(members[2])
    .bind(channel_id)
    .execute(&pool)
    .await;
    let _ = sqlx::query(
        "UPDATE privchat_channel_read_cursor SET updated_at = NOW() WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(members[1])
    .bind(channel_id)
    .execute(&pool)
    .await;

    let rows = list_read_members_projection(&pool, channel_id, message_pts, &members).await;
    let ids: Vec<i64> = rows.iter().map(|(uid, _)| *uid).collect();
    assert_eq!(ids, vec![members[1], members[2]]);
    assert!(rows.iter().all(|(_, pts)| *pts >= message_pts));

    for uid in members.iter().chain(std::iter::once(&non_member)) {
        cleanup(&pool, *uid, channel_id).await;
    }
}

#[test]
fn read_stats_respects_disabled_policy() {
    assert!(ensure_read_stats_allowed(ReadReceiptMode::Disabled).is_err());
}

#[test]
fn read_list_respects_disabled_policy() {
    assert!(ensure_read_list_allowed(ReadReceiptMode::Disabled).is_err());
}

#[test]
fn read_stats_respects_count_only_policy() {
    assert!(ensure_read_stats_allowed(ReadReceiptMode::CountOnly).is_ok());
}

#[test]
fn read_list_respects_count_only_policy() {
    assert!(ensure_read_list_allowed(ReadReceiptMode::CountOnly).is_err());
}
