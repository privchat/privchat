use sqlx::postgres::PgPoolOptions;
use sqlx::Row;

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

async fn cleanup_user(pool: &sqlx::PgPool, user_id: i64) {
    let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await;
}

async fn cleanup_group(pool: &sqlx::PgPool, group_id: i64) {
    let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
        .bind(group_id)
        .execute(pool)
        .await;
}

async fn cleanup_channel(pool: &sqlx::PgPool, channel_id: i64) {
    let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
        .bind(channel_id)
        .execute(pool)
        .await;
}

async fn ensure_user(pool: &sqlx::PgPool, user_id: i64, username: &str) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, display_name)
        VALUES ($1, $2, $2)
        ON CONFLICT (user_id) DO UPDATE
        SET username = EXCLUDED.username,
            display_name = EXCLUDED.display_name
        "#,
    )
    .bind(user_id)
    .bind(username)
    .execute(pool)
    .await
    .expect("ensure user");
}

async fn ensure_group(pool: &sqlx::PgPool, group_id: i64, owner_id: i64, name: &str) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_groups (group_id, owner_id, name, description)
        VALUES ($1, $2, $3, 'sync-version-test')
        ON CONFLICT (group_id) DO UPDATE
        SET owner_id = EXCLUDED.owner_id,
            name = EXCLUDED.name
        "#,
    )
    .bind(group_id)
    .bind(owner_id)
    .bind(name)
    .execute(pool)
    .await
    .expect("ensure group");
}

async fn ensure_channel(pool: &sqlx::PgPool, channel_id: i64, group_id: i64) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_channels (channel_id, channel_type, group_id)
        VALUES ($1, 1, $2)
        ON CONFLICT (channel_id) DO UPDATE
        SET group_id = EXCLUDED.group_id
        "#,
    )
    .bind(channel_id)
    .bind(group_id)
    .execute(pool)
    .await
    .expect("ensure channel");
}

async fn ensure_user_channel(pool: &sqlx::PgPool, user_id: i64, channel_id: i64) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_user_channels (user_id, channel_id, unread_count, is_pinned, is_muted)
        VALUES ($1, $2, 0, false, false)
        ON CONFLICT (user_id, channel_id) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(channel_id)
    .execute(pool)
    .await
    .expect("ensure user channel");
}

async fn ensure_friendship(pool: &sqlx::PgPool, user_id: i64, friend_id: i64) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_friendships (user_id, friend_id, status)
        VALUES ($1, $2, 0)
        ON CONFLICT (user_id, friend_id) DO UPDATE
        SET status = EXCLUDED.status
        "#,
    )
    .bind(user_id)
    .bind(friend_id)
    .execute(pool)
    .await
    .expect("ensure friendship");
}

async fn ensure_group_member(pool: &sqlx::PgPool, group_id: i64, user_id: i64) {
    let _ = sqlx::query(
        r#"
        INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, left_at)
        VALUES ($1, $2, 2, now_millis(), NULL)
        ON CONFLICT (group_id, user_id) DO UPDATE
        SET left_at = NULL, role = 2
        "#,
    )
    .bind(group_id)
    .bind(user_id)
    .execute(pool)
    .await
    .expect("ensure group member");
}

async fn current_sync_version(
    pool: &sqlx::PgPool,
    sql: &str,
    bind1: i64,
    bind2: Option<i64>,
) -> i64 {
    let mut query = sqlx::query(sql).bind(bind1);
    if let Some(value) = bind2 {
        query = query.bind(value);
    }
    query
        .fetch_one(pool)
        .await
        .expect("fetch sync version")
        .get::<i64, _>("sync_version")
}

#[tokio::test]
async fn user_sync_version_advances_on_profile_update() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip user_sync_version_advances_on_profile_update: DATABASE_URL not configured");
        return;
    };
    let user_id = 9_910_001_i64;
    cleanup_user(&pool, user_id).await;
    ensure_user(&pool, user_id, "sync_version_user_9910001").await;

    let before = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_users WHERE user_id = $1",
        user_id,
        None,
    )
    .await;

    let _ = sqlx::query("UPDATE privchat_users SET display_name = $2 WHERE user_id = $1")
        .bind(user_id)
        .bind("updated-display-name")
        .execute(&pool)
        .await
        .expect("update user");

    let after = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_users WHERE user_id = $1",
        user_id,
        None,
    )
    .await;
    assert!(after > before, "user sync_version should advance");

    cleanup_user(&pool, user_id).await;
}

#[tokio::test]
async fn channel_and_user_channel_sync_versions_advance_on_state_update() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skip channel_and_user_channel_sync_versions_advance_on_state_update: DATABASE_URL not configured");
        return;
    };
    let owner_id = 9_910_002_i64;
    let group_id = 9_920_002_i64;
    let channel_id = 9_930_002_i64;

    cleanup_channel(&pool, channel_id).await;
    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, owner_id).await;

    ensure_user(&pool, owner_id, "sync_version_owner_9910002").await;
    ensure_group(&pool, group_id, owner_id, "sync-version-group").await;
    ensure_channel(&pool, channel_id, group_id).await;
    ensure_user_channel(&pool, owner_id, channel_id).await;

    let before_channel = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_channels WHERE channel_id = $1",
        channel_id,
        None,
    )
    .await;
    let before_user_channel = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_user_channels WHERE user_id = $1 AND channel_id = $2",
        owner_id,
        Some(channel_id),
    )
    .await;

    let _ = sqlx::query(
        "UPDATE privchat_channels SET last_message_at = now_millis() WHERE channel_id = $1",
    )
    .bind(channel_id)
    .execute(&pool)
    .await
    .expect("update channel");
    let _ = sqlx::query(
        "UPDATE privchat_user_channels SET is_pinned = true, is_muted = true WHERE user_id = $1 AND channel_id = $2",
    )
    .bind(owner_id)
    .bind(channel_id)
    .execute(&pool)
    .await
    .expect("update user channel");

    let after_channel = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_channels WHERE channel_id = $1",
        channel_id,
        None,
    )
    .await;
    let after_user_channel = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_user_channels WHERE user_id = $1 AND channel_id = $2",
        owner_id,
        Some(channel_id),
    )
    .await;

    assert!(
        after_channel > before_channel,
        "channel sync_version should advance"
    );
    assert!(
        after_user_channel > before_user_channel,
        "user_channel sync_version should advance"
    );

    cleanup_channel(&pool, channel_id).await;
    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, owner_id).await;
}

#[tokio::test]
async fn friend_and_group_sync_versions_advance_on_update() {
    let Some(pool) = open_test_pool().await else {
        eprintln!(
            "skip friend_and_group_sync_versions_advance_on_update: DATABASE_URL not configured"
        );
        return;
    };
    let user_a = 9_910_003_i64;
    let user_b = 9_910_004_i64;
    let group_id = 9_920_003_i64;

    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, user_a).await;
    cleanup_user(&pool, user_b).await;

    ensure_user(&pool, user_a, "sync_version_friend_a").await;
    ensure_user(&pool, user_b, "sync_version_friend_b").await;
    ensure_friendship(&pool, user_a, user_b).await;
    ensure_group(&pool, group_id, user_a, "group-before-update").await;

    let before_friend = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_friendships WHERE user_id = $1 AND friend_id = $2",
        user_a,
        Some(user_b),
    )
    .await;
    let before_group = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_groups WHERE group_id = $1",
        group_id,
        None,
    )
    .await;

    let _ = sqlx::query(
        "UPDATE privchat_friendships SET status = 1, request_message = 'accepted' WHERE user_id = $1 AND friend_id = $2",
    )
    .bind(user_a)
    .bind(user_b)
    .execute(&pool)
    .await
    .expect("update friendship");
    let _ = sqlx::query("UPDATE privchat_groups SET name = $2 WHERE group_id = $1")
        .bind(group_id)
        .bind("group-after-update")
        .execute(&pool)
        .await
        .expect("update group");

    let after_friend = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_friendships WHERE user_id = $1 AND friend_id = $2",
        user_a,
        Some(user_b),
    )
    .await;
    let after_group = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_groups WHERE group_id = $1",
        group_id,
        None,
    )
    .await;

    assert!(
        after_friend > before_friend,
        "friend sync_version should advance"
    );
    assert!(
        after_group > before_group,
        "group sync_version should advance"
    );

    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, user_a).await;
    cleanup_user(&pool, user_b).await;
}

#[tokio::test]
async fn group_member_sync_version_advances_on_soft_delete() {
    let Some(pool) = open_test_pool().await else {
        eprintln!(
            "skip group_member_sync_version_advances_on_soft_delete: DATABASE_URL not configured"
        );
        return;
    };
    let owner_id = 9_910_005_i64;
    let member_id = 9_910_006_i64;
    let group_id = 9_920_005_i64;

    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, owner_id).await;
    cleanup_user(&pool, member_id).await;

    ensure_user(&pool, owner_id, "sync_version_group_owner").await;
    ensure_user(&pool, member_id, "sync_version_group_member").await;
    ensure_group(&pool, group_id, owner_id, "group-member-sync-version").await;
    ensure_group_member(&pool, group_id, member_id).await;

    let before = current_sync_version(
        &pool,
        "SELECT sync_version FROM privchat_group_members WHERE group_id = $1 AND user_id = $2",
        group_id,
        Some(member_id),
    )
    .await;

    let _ = sqlx::query(
        "UPDATE privchat_group_members SET left_at = now_millis() WHERE group_id = $1 AND user_id = $2",
    )
    .bind(group_id)
    .bind(member_id)
    .execute(&pool)
    .await
    .expect("soft delete group member");

    let row = sqlx::query(
        "SELECT sync_version, left_at FROM privchat_group_members WHERE group_id = $1 AND user_id = $2",
    )
    .bind(group_id)
    .bind(member_id)
    .fetch_one(&pool)
    .await
    .expect("fetch group member");
    let after = row.get::<i64, _>("sync_version");
    let left_at = row.get::<Option<i64>, _>("left_at");

    assert!(after > before, "group_member sync_version should advance");
    assert!(
        left_at.is_some(),
        "group_member tombstone should set left_at"
    );

    cleanup_group(&pool, group_id).await;
    cleanup_user(&pool, owner_id).await;
    cleanup_user(&pool, member_id).await;
}
