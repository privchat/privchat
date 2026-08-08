// 发送权限策略的行为矩阵（真库）。
//
// 🔴 为什么必须是行为测试、必须打真库：这套策略是**全站发送热路径**，
// 从 `send_message_handler` 抽出来时只要顺序错一档，后果就是「被拉黑的人还能发」
// 或者「数据库抖一下禁言失效」。错误码映射那种轻量测试保护不了这些。
//
// 也不允许在测试里另写一份判定——那样测的是抄件，不是产品。这里调的是
// `service::send_authorization::authorize_send_to_channel` 本身。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::config::CacheConfig;
use privchat::infra::CacheManager;
use privchat::repository::PgChannelRepository;
use privchat::service::send_authorization::{
    authorize_send_to_channel, SendAuthorizationDeps, SendRefusal,
};
use privchat::service::{BlacklistService, ChannelService, FriendService, PrivacyService};

const OWNER: i64 = 9_960_001;
const MEMBER: i64 = 9_960_002;
const STRANGER: i64 = 9_960_003;
const GROUP_ID: i64 = 9_961_001;
const DM_CHANNEL: i64 = 9_961_002;

fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn deps() -> Option<(SendAuthorizationDeps, Arc<sqlx::PgPool>)> {
    let url = privchat::require_test_database_url()?;
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(6)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    );
    let cache = Arc::new(
        CacheManager::new(CacheConfig::default())
            .await
            .expect("cache"),
    );
    let blacklist_service = Arc::new(BlacklistService::new(cache.clone()));
    let channel_service = Arc::new(ChannelService::new_with_repository(Arc::new(
        PgChannelRepository::new(pool.clone()),
    )));
    let friend_service = Arc::new(FriendService::new(pool.clone()));
    Some((
        SendAuthorizationDeps {
            channel_service: channel_service.clone(),
            friend_service: friend_service.clone(),
            blacklist_service: blacklist_service.clone(),
            privacy_service: Arc::new(PrivacyService::new(
                cache,
                channel_service,
                friend_service,
            )),
        },
        pool,
    ))
}

async fn ensure_user(pool: &sqlx::PgPool, user_id: i64, username: &str) {
    sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, display_name, qr_key)
        VALUES ($1, $2, $2, $3)
        ON CONFLICT (user_id) DO NOTHING
        "#,
    )
    .bind(user_id)
    .bind(username)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool)
    .await
    .expect("ensure user");
}

async fn cleanup(pool: &sqlx::PgPool) {
    for sql in [
        "DELETE FROM privchat_friendships WHERE user_id = ANY($1) OR friend_id = ANY($1)",
        "DELETE FROM privchat_group_members WHERE group_id = $2",
        "DELETE FROM privchat_channel_participants WHERE channel_id = ANY($3)",
        "DELETE FROM privchat_channels WHERE channel_id = ANY($3)",
        "DELETE FROM privchat_groups WHERE group_id = $2",
    ] {
        let _ = sqlx::query(sql)
            .bind(vec![OWNER, MEMBER, STRANGER])
            .bind(GROUP_ID)
            .bind(vec![GROUP_ID, DM_CHANNEL])
            .execute(pool)
            .await;
    }
}

async fn seed_group(pool: &sqlx::PgPool, channel_service: &ChannelService) {
    sqlx::query(
        r#"
        INSERT INTO privchat_groups (group_id, name, owner_id, member_count, qr_key)
        VALUES ($1, 'send-authz', $2, 2, $3)
        ON CONFLICT (group_id) DO NOTHING
        "#,
    )
    .bind(GROUP_ID)
    .bind(OWNER)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool)
    .await
    .expect("group");
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, group_id) VALUES ($1, 1, $1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(GROUP_ID)
    .execute(pool)
    .await
    .expect("channel");
    // 角色编码：Owner=0 / Admin=1 / Member=2（`MemberRole`）。
    // 写错会让「普通成员」变成管理员，于是全员禁言测试假绿。
    for (user_id, role) in [(OWNER, 0i16), (MEMBER, 2i16)] {
        sqlx::query(
            r#"
            INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, updated_at)
            VALUES ($1, $2, $3, now_millis(), now_millis())
            ON CONFLICT DO NOTHING
            "#,
        )
        .bind(GROUP_ID)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("member");
        sqlx::query(
            r#"
            INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at)
            VALUES ($1, $2, $3, now_millis())
            ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL
            "#,
        )
        .bind(GROUP_ID)
        .bind(user_id)
        .bind(role)
        .execute(pool)
        .await
        .expect("participant");
    }
}

/// 群会话矩阵：非成员 / 全员禁言（含群主豁免）。
#[tokio::test]
async fn group_send_policy_matrix() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    for (uid, name) in [
        (OWNER, "sa_owner"),
        (MEMBER, "sa_member"),
        (STRANGER, "sa_stranger"),
    ] {
        ensure_user(&pool, uid, name).await;
    }
    seed_group(&pool, &deps.channel_service).await;

    let channel = deps
        .channel_service
        .get_channel_opt(GROUP_ID as u64)
        .await
        .expect("channel loaded");

    assert_eq!(
        authorize_send_to_channel(&deps, &channel, STRANGER as u64).await,
        Err(SendRefusal::NotAMember),
        "非成员不能发言",
    );
    assert!(
        authorize_send_to_channel(&deps, &channel, MEMBER as u64)
            .await
            .is_ok(),
        "普通成员默认可以发言",
    );

    // 全员禁言：普通成员被拦，群主豁免。
    sqlx::query("UPDATE privchat_groups SET all_muted = true WHERE group_id = $1")
        .bind(GROUP_ID)
        .execute(pool.as_ref())
        .await
        .expect("mute all");
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, MEMBER as u64).await,
        Err(SendRefusal::GroupAllMuted),
        "全员禁言拦住普通成员",
    );
    assert!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64)
            .await
            .is_ok(),
        "群主豁免全员禁言",
    );

    cleanup(&pool).await;
}

/// 🔴 私聊矩阵的核心一条：**仍是好友、但已被拉黑**必须拦住。
///
/// 拉黑不解除好友关系。原实现「好友直接放行」把这种情况漏了过去，
/// 而这正是拉黑要挡的那件事（BLACKLIST_SPEC §5 的流程里没有好友快捷放行）。
#[tokio::test]
async fn a_blocked_friend_still_cannot_send() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;

    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id) \
         VALUES ($1, 0, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(DM_CHANNEL)
    .bind(OWNER)
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("dm channel");
    for uid in [OWNER, MEMBER] {
        sqlx::query(
            "INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at) \
             VALUES ($1, $2, 2, now_millis()) ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
        )
        .bind(DM_CHANNEL)
        .bind(uid)
        .execute(pool.as_ref())
        .await
        .expect("dm participant");
    }

    // 建立好友关系，再让接收方拉黑发送方。
    // 建立好友关系，再让接收方拉黑发送方。
    sqlx::query(
        "INSERT INTO privchat_friendships (user_id, friend_id, status, created_at, updated_at) \
         VALUES ($1, $2, 1, now_millis(), now_millis()), ($2, $1, 1, now_millis(), now_millis()) \
         ON CONFLICT DO NOTHING",
    )
    .bind(OWNER)
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("befriend");
    deps.blacklist_service
        .add_to_blacklist(MEMBER as u64, OWNER as u64, None)
        .await
        .expect("block");

    let channel = deps
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm loaded");

    assert_eq!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64).await,
        Err(SendRefusal::BlockedByPeer),
        "仍是好友但被拉黑 → 必须拦住（拉黑先于好友判定）",
    );

    cleanup(&pool).await;
}
