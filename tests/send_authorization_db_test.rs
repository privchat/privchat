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
    let blacklist_service = Arc::new(BlacklistService::new(pool.clone(), cache.clone()));
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

/// 清理。
///
/// 🔴 每条 SQL **各自绑定自己的参数**，并且 `expect` 结果。
/// 上一版给每条语句都绑了三组参数、又用 `let _ =` 吞掉错误，于是 Postgres
/// 因参数个数不符全部拒绝——清理从未真正执行过。配上固定主键和
/// `ON CONFLICT DO NOTHING`，测试就会复用上一轮的残留状态，绿得毫无意义。
async fn cleanup(pool: &sqlx::PgPool) {
    let users = vec![OWNER, MEMBER, STRANGER];
    let channels = vec![GROUP_ID, DM_CHANNEL];

    sqlx::query("DELETE FROM privchat_blacklist WHERE user_id = ANY($1) OR blocked_user_id = ANY($1)")
        .bind(&users)
        .execute(pool)
        .await
        .expect("clean blacklist");
    sqlx::query("DELETE FROM privchat_friendships WHERE user_id = ANY($1) OR friend_id = ANY($1)")
        .bind(&users)
        .execute(pool)
        .await
        .expect("clean friendships");
    sqlx::query("DELETE FROM privchat_group_members WHERE group_id = $1")
        .bind(GROUP_ID)
        .execute(pool)
        .await
        .expect("clean group members");
    sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id = ANY($1)")
        .bind(&channels)
        .execute(pool)
        .await
        .expect("clean participants");
    sqlx::query("DELETE FROM privchat_channels WHERE channel_id = ANY($1)")
        .bind(&channels)
        .execute(pool)
        .await
        .expect("clean channels");
    sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
        .bind(GROUP_ID)
        .execute(pool)
        .await
        .expect("clean group");
}

async fn seed_group(pool: &sqlx::PgPool) {
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
    seed_group(&pool).await;

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
async fn seed_dm(pool: &sqlx::PgPool) {
    sqlx::query(
        "INSERT INTO privchat_channels (channel_id, channel_type, direct_user1_id, direct_user2_id) \
         VALUES ($1, 0, $2, $3) ON CONFLICT DO NOTHING",
    )
    .bind(DM_CHANNEL)
    .bind(OWNER)
    .bind(MEMBER)
    .execute(pool)
    .await
    .expect("dm channel");
    for uid in [OWNER, MEMBER] {
        sqlx::query(
            "INSERT INTO privchat_channel_participants (channel_id, user_id, role, joined_at) \
             VALUES ($1, $2, 2, now_millis()) \
             ON CONFLICT (channel_id, user_id) DO UPDATE SET left_at = NULL",
        )
        .bind(DM_CHANNEL)
        .bind(uid)
        .execute(pool)
        .await
        .expect("dm participant");
    }
}

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

/// 个人禁言：被禁言的成员发不出，同群其他人不受影响。
#[tokio::test]
async fn a_muted_member_is_refused_while_others_are_not() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_group(&pool).await;

    // 禁言只有 mute_until 一列：NULL = 未禁言，未来时间 = 禁言中。
    sqlx::query(
        "UPDATE privchat_channel_participants SET mute_until = now_millis() + 3600000 \
         WHERE channel_id = $1 AND user_id = $2",
    )
    .bind(GROUP_ID)
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("mute member");

    let channel = deps
        .channel_service
        .get_channel_opt(GROUP_ID as u64)
        .await
        .expect("channel");
    assert!(
        matches!(
            authorize_send_to_channel(&deps, &channel, MEMBER as u64).await,
            Err(SendRefusal::MemberMuted(_))
        ),
        "被禁言的成员必须被拦住",
    );
    assert!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64)
            .await
            .is_ok(),
        "禁言只作用于被禁的那个人",
    );

    cleanup(&pool).await;
}

/// 双向拉黑的**两个方向**要分别报告：被对方拉黑 vs 自己拉黑了对方。
/// 文案与后续动作不同，压成同一个码就分不出来了。
#[tokio::test]
async fn both_directions_of_blocking_are_reported_distinctly() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;
    let channel = deps
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm");

    // 我拉黑对方
    deps.blacklist_service
        .add_to_blacklist(OWNER as u64, MEMBER as u64, None)
        .await
        .expect("block peer");
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64).await,
        Err(SendRefusal::PeerInMyBlacklist),
    );
    // 对方也拉黑我 —— 「被拉黑」优先，因为那是对方的意愿
    deps.blacklist_service
        .add_to_blacklist(MEMBER as u64, OWNER as u64, None)
        .await
        .expect("blocked by peer");
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64).await,
        Err(SendRefusal::BlockedByPeer),
    );

    cleanup(&pool).await;
}

/// 非好友 + 对方「仅接收好友消息」→ 拒绝。
#[tokio::test]
async fn a_stranger_is_refused_when_the_peer_only_accepts_friends() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;
    sqlx::query(
        r#"
        INSERT INTO privchat_users (user_id, username, qr_key, privacy_settings)
        VALUES ($1, 'sa_member', $2, '{"allow_receive_message_from_non_friend": false}')
        ON CONFLICT (user_id) DO UPDATE
            SET privacy_settings = '{"allow_receive_message_from_non_friend": false}'
        "#,
    )
    .bind(MEMBER)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool.as_ref())
    .await
    .expect("privacy");

    let channel = deps
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm");
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64).await,
        Err(SendRefusal::PeerRejectsNonFriends),
        "非好友遇到「仅接收好友消息」必须被拦",
    );

    cleanup(&pool).await;
}

/// 🔴 限制性策略查不到时**拒绝**，而且报的是可重试的服务异常，
/// 不是「无权」也不是「已禁言」——那两种都会让用户走错路。
#[tokio::test]
async fn a_policy_lookup_failure_refuses_as_a_retryable_service_error() {
    use privchat_protocol::error_code::ErrorCode;

    let _guard = fixture_lock().lock().await;
    let Some((good, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;
    let channel = good
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm");

    // 黑名单查询打到一个连不上的库（黑名单已是 DB 真源，这条故障是真的）。
    let broken_pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_millis(300))
            .connect_lazy("postgres://nobody@127.0.0.1:59999/nope")
            .expect("lazy pool"),
    );
    let cache = Arc::new(
        CacheManager::new(CacheConfig::default())
            .await
            .expect("cache"),
    );
    let broken = SendAuthorizationDeps {
        blacklist_service: Arc::new(BlacklistService::new(broken_pool, cache)),
        ..good.clone()
    };

    assert_eq!(
        authorize_send_to_channel(&broken, &channel, OWNER as u64).await,
        Err(SendRefusal::PolicyUnavailable),
    );
    assert_eq!(
        SendRefusal::PolicyUnavailable.error_code(),
        ErrorCode::ServiceUnavailable,
        "必须落在两端 SDK 的可重试白名单里；InternalError(4) 不在白名单，\
         会让「稍后重试」的文案配上终局失败的行为",
    );

    cleanup(&pool).await;
}
