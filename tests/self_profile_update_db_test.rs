// 自助改资料的真库测试（account/profile/update）。
//
// 为什么要真库：这条路径此前是个「返回 success 但什么都不写」的桩，而桩也能让
// 任何纯函数测试通过。真正要证明的两件事都只有数据库能回答——值写进去了，
// 以及 sync_version 被推进了（多端就是靠它把新昵称同步过去的）。
use sqlx::Row;

async fn pool() -> Option<sqlx::PgPool> {
    let url = privchat::require_test_database_url()?;
    sqlx::PgPool::connect(&url).await.ok()
}

async fn seed_user(p: &sqlx::PgPool, user_id: i64, username: &str) {
    sqlx::query(
        "INSERT INTO privchat_users (user_id, username, qr_key, created_at, updated_at)
         VALUES ($1, $2, 'qr-' || $1::text, 0, 0)
         ON CONFLICT (user_id) DO UPDATE SET username = EXCLUDED.username, display_name = NULL",
    )
    .bind(user_id)
    .bind(username)
    .execute(p)
    .await
    .expect("seed user");
}

async fn read(p: &sqlx::PgPool, user_id: i64) -> (Option<String>, i64) {
    let row = sqlx::query("SELECT display_name, sync_version FROM privchat_users WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(p)
        .await
        .expect("read user");
    (row.get("display_name"), row.get("sync_version"))
}

#[tokio::test]
async fn setting_a_nickname_persists_and_bumps_sync_version() {
    let Some(p) = pool().await else { return };
    let user_id = 910_001i64;
    seed_user(&p, user_id, "selfprofile1").await;
    let (before_name, before_version) = read(&p, user_id).await;
    assert_eq!(before_name, None, "新账号没有昵称");

    let service = privchat::service::UserService::new(std::sync::Arc::new(
        privchat::repository::UserRepository::new(std::sync::Arc::new(p.clone())),
    ));
    service
        .update_own_profile(user_id as u64, Some("张三".to_string()), None)
        .await
        .expect("update own profile");

    let (after_name, after_version) = read(&p, user_id).await;
    assert_eq!(after_name, Some("张三".to_string()));
    // 🔴 多端同步的唯一游标。写了值但 sync_version 没动 = 别的设备永远看不到。
    assert!(
        after_version > before_version,
        "sync_version 没有推进：{before_version} → {after_version}"
    );
}

/// 空昵称是「清空」，而且不能顺手把用户名一起清了。
#[tokio::test]
async fn clearing_the_nickname_keeps_the_username() {
    let Some(p) = pool().await else { return };
    let user_id = 910_002i64;
    seed_user(&p, user_id, "selfprofile2").await;

    let service = privchat::service::UserService::new(std::sync::Arc::new(
        privchat::repository::UserRepository::new(std::sync::Arc::new(p.clone())),
    ));
    service
        .update_own_profile(user_id as u64, Some("有昵称".to_string()), None)
        .await
        .expect("set");
    service
        .update_own_profile(user_id as u64, Some("   ".to_string()), None)
        .await
        .expect("clear");

    let row = sqlx::query("SELECT username, display_name FROM privchat_users WHERE user_id = $1")
        .bind(user_id)
        .fetch_one(&p)
        .await
        .expect("read");
    assert_eq!(row.get::<Option<String>, _>("display_name"), None);
    assert_eq!(
        row.get::<Option<String>, _>("username"),
        Some("selfprofile2".to_string()),
        "清昵称不得动用户名——那是登录凭证"
    );
}

/// 只改头像时不能把昵称抹掉：`None` 是「本次不改」，不是「清空」。
#[tokio::test]
async fn updating_only_the_avatar_leaves_the_nickname_alone() {
    let Some(p) = pool().await else { return };
    let user_id = 910_003i64;
    seed_user(&p, user_id, "selfprofile3").await;

    let service = privchat::service::UserService::new(std::sync::Arc::new(
        privchat::repository::UserRepository::new(std::sync::Arc::new(p.clone())),
    ));
    service
        .update_own_profile(user_id as u64, Some("保留我".to_string()), None)
        .await
        .expect("set nickname");
    service
        .update_own_profile(
            user_id as u64,
            None,
            Some("https://example.com/a.png".to_string()),
        )
        .await
        .expect("set avatar");

    let row =
        sqlx::query("SELECT display_name, avatar_url FROM privchat_users WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(&p)
            .await
            .expect("read");
    assert_eq!(
        row.get::<Option<String>, _>("display_name"),
        Some("保留我".to_string())
    );
    assert_eq!(
        row.get::<Option<String>, _>("avatar_url"),
        Some("https://example.com/a.png".to_string())
    );
}
