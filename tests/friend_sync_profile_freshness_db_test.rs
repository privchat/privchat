//! A friend-sync page must read profiles from the database, not from the profile cache.
//!
//! Production regression, 2026-09-09: a user bound an invite code, the server created
//! the friendship and cached the peer profile while the nickname was still unset; the
//! user set a nickname seconds later. `update_user_admin` wrote the database row and the
//! trigger bumped `sync_version`, but nothing invalidated the cache — so friend sync kept
//! serving `nickname: ""` and every client, including a fresh install, rendered the DM
//! title as the raw username. ENTITY_INVALIDATION_SYNC_SPEC §2.1 makes pull the
//! authority; an authoritative read that can serve a stale cache entry can never
//! converge, no matter how often the client retries.
//!
//! This test poisons the cache on purpose and asserts the sync page ignores it. That is
//! also what makes the classic read-refill race harmless *for synchronisation*: a
//! concurrent reader can still refill the cache with a value it read before the write
//! committed, but nothing on the sync path consults that cache.
//!
//! Requires PRIVCHAT_TEST_DATABASE_URL (or DATABASE_URL); skipped otherwise.

use std::sync::Arc;

use privchat::infra::{CacheManager, CachedUserProfile};
use privchat::repository::UserRepository;
use privchat::service::FriendService;
use sqlx::postgres::PgPoolOptions;

async fn open_test_pool() -> Option<sqlx::PgPool> {
    let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()?;
    PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .ok()
}

async fn ensure_user(pool: &sqlx::PgPool, user_id: i64, username: &str, display_name: &str) {
    let qr_key = privchat::rpc::qr::generate_qr_key();
    sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, display_name, qr_key)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (user_id) DO UPDATE
        SET username = EXCLUDED.username,
            display_name = EXCLUDED.display_name
        "#,
    )
    .bind(user_id)
    .bind(username)
    .bind(display_name)
    .bind(&qr_key)
    .execute(pool)
    .await
    .expect("ensure user");
}

async fn ensure_friendship(pool: &sqlx::PgPool, a: i64, b: i64) {
    for (x, y) in [(a, b), (b, a)] {
        sqlx::query(
            r#"
            INSERT INTO privchat_friendships (user_id, friend_id, status)
            VALUES ($1, $2, 1)
            ON CONFLICT (user_id, friend_id) DO UPDATE SET status = 1
            "#,
        )
        .bind(x)
        .bind(y)
        .execute(pool)
        .await
        .expect("ensure friendship");
    }
}

async fn cleanup(pool: &sqlx::PgPool, ids: &[i64]) {
    for id in ids {
        let _ = sqlx::query("DELETE FROM privchat_friendships WHERE user_id = $1 OR friend_id = $1")
            .bind(id)
            .execute(pool)
            .await;
        let _ = sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
            .bind(id)
            .execute(pool)
            .await;
    }
}

#[tokio::test]
async fn friend_sync_serves_the_database_profile_even_when_the_cache_is_stale() {
    let Some(pool) = open_test_pool().await else {
        eprintln!("skipping: no PRIVCHAT_TEST_DATABASE_URL/DATABASE_URL");
        return;
    };

    let viewer: i64 = 990_101;
    let peer: i64 = 990_102;
    cleanup(&pool, &[viewer, peer]).await;
    ensure_user(&pool, viewer, "freshviewer", "Viewer").await;
    // The peer's authoritative name is the nickname they just set.
    ensure_user(&pool, peer, "freshpeer", "RealNickname").await;
    ensure_friendship(&pool, viewer, peer).await;

    let pool_arc = Arc::new(pool.clone());
    let user_repository = Arc::new(UserRepository::new(pool_arc.clone()));
    let cache_manager = Arc::new(
        CacheManager::new(Default::default())
            .await
            .expect("cache manager"),
    );

    // Poison the cache with what it looked like before the nickname existed — exactly the
    // shape that broke production.
    cache_manager
        .set_user_profile(
            peer as u64,
            CachedUserProfile {
                user_id: peer.to_string(),
                username: "freshpeer".to_string(),
                nickname: String::new(),
                avatar_url: None,
                user_type: 0,
                phone: None,
                email: None,
            },
        )
        .await
        .expect("poison cache");

    let friend_service = FriendService::new(pool_arc.clone());
    let page = friend_service
        .sync_entities_page(viewer as u64, Some(0), None, 50, &user_repository, &cache_manager)
        .await
        .expect("sync page");

    let item = page
        .items
        .iter()
        .find(|item| item.entity_id == peer.to_string())
        .expect("peer present in friend sync page");
    let payload = item.payload.as_ref().expect("payload");
    let embedded = payload.get("user").expect("embedded user");

    assert_eq!(
        embedded.get("nickname").and_then(|v| v.as_str()),
        Some("RealNickname"),
        "friend sync must read the profile from the database, not the stale cache",
    );

    // The embedded profile also has to carry the *user's own* version, so a client can
    // order it against the `user` entity stream instead of against the friendship's.
    let embedded_version = embedded
        .get("version")
        .and_then(|v| v.as_i64())
        .expect("embedded user carries its own sync_version");
    let user_sync_version: i64 =
        sqlx::query_scalar("SELECT sync_version FROM privchat_users WHERE user_id = $1")
            .bind(peer)
            .fetch_one(&pool)
            .await
            .expect("read user sync_version");
    assert_eq!(
        embedded_version, user_sync_version,
        "embedded profile version must be the user's, not the friendship's",
    );

    cleanup(&pool, &[viewer, peer]).await;
}
