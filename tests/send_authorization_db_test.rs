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
    // 🔴 隐私设置必须一起重置：用例共享固定 UID，隐私用例写下的 false
    // 会留给后面的用例，让「非好友也能发」那类断言以错误的原因通过或失败。
    sqlx::query("UPDATE privchat_users SET privacy_settings = '{}' WHERE user_id = ANY($1)")
        .bind(&users)
        .execute(pool)
        .await
        .expect("reset privacy settings");
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

/// 群设为「仅群主/管理员可发言」（公告群）→ 普通成员被拦，管理层不受限。
///
/// 这条策略此前**在生产上不可达**：`allow_member_post` 只活在 `ChannelSettings`
/// 这份内存缓存里，没有写入入口，也没有任何东西让它跨重启存活——读它的代码存在，
/// 值永远是默认的 true。所以这里走的是真正的写入路径（`update_group_policy`，
/// 即 `group/settings/update` 落库用的那个），不是手动改内存字段。
#[tokio::test]
async fn a_group_that_forbids_member_posting_refuses_ordinary_members() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_group(&pool).await;

    deps.channel_service
        .update_group_policy(GROUP_ID as u64, None, None, None, None, None, Some(false), None)
        .await
        .expect("落库 allow_member_post=false");

    let channel = deps
        .channel_service
        .get_channel_opt(GROUP_ID as u64)
        .await
        .expect("channel");

    assert_eq!(
        authorize_send_to_channel(&deps, &channel, MEMBER as u64).await,
        Err(SendRefusal::ChannelForbidsPosting),
        "只读群里普通成员必须被拦",
    );
    assert!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64)
            .await
            .is_ok(),
        "群主不受 allow_member_post 限制",
    );

    cleanup(&pool).await;
}

/// 🔴 只读设置必须跨重启存活。
///
/// 这正是它此前形同虚设的地方：限制活在内存里，进程一重启就回到默认的「允许发言」，
/// 而默认值是放开的一侧。一个限制性策略只要重启就消失，等于没有这个策略。
#[tokio::test]
async fn a_read_only_group_survives_a_fresh_service_instance() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_group(&pool).await;

    deps.channel_service
        .update_group_policy(GROUP_ID as u64, None, None, None, None, None, Some(false), None)
        .await
        .expect("落库 allow_member_post=false");

    // 全新实例：自带空的频道缓存，只能从 DB 读。
    let (fresh, _) = deps_fresh().await.expect("fresh deps");
    let channel = fresh
        .channel_service
        .get_channel_opt(GROUP_ID as u64)
        .await
        .expect("channel");
    assert_eq!(
        authorize_send_to_channel(&fresh, &channel, MEMBER as u64).await,
        Err(SendRefusal::ChannelForbidsPosting),
        "重启后只读群仍然只读——限制不能随进程一起消失",
    );

    cleanup(&pool).await;
}

/// 「通过名片添加」开关必须真的落库。
///
/// 🔴 它此前是**静默丢弃**：App 有开关、也发了 `allow_add_by_card`，但 protocol 请求体、
/// server 映射、get 响应三处都没有这个字段。用户关掉后 UI 先显示成功，重新读又回到
/// 默认的 true——不是「没保存」，是隐私开关看起来生效、实际一直开着。
#[tokio::test]
async fn turning_off_add_by_card_is_actually_persisted() {
    use privchat::service::privacy_service::PrivacySettingsUpdate;

    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, MEMBER, "sa_member").await;

    let updated = deps
        .privacy_service
        .update_privacy_settings(
            MEMBER as u64,
            PrivacySettingsUpdate {
                allow_add_by_card: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("update");
    assert!(!updated.allow_add_by_card, "更新响应必须回显新值");

    // 全新实例：空缓存，只能从 DB 读回来。
    let (fresh, _) = deps_fresh().await.expect("fresh deps");
    let reread = fresh
        .privacy_service
        .get_or_create_privacy_settings(MEMBER as u64)
        .await
        .expect("re-read");
    assert!(
        !reread.allow_add_by_card,
        "重新读取必须仍是关闭——回到 true 就是开关形同虚设",
    );

    cleanup(&pool).await;
}

/// 隐私存储损坏（脏 JSON）→ 拒绝且可重试，**不能**回落成「允许非好友消息」。
#[tokio::test]
async fn corrupt_privacy_storage_refuses_instead_of_falling_back_to_permissive() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;

    // 字段类型不对 = 脏数据。缺字段是正常的增量存储，不算脏。
    sqlx::query(
        r#"UPDATE privchat_users
           SET privacy_settings = '{"allow_receive_message_from_non_friend": "nope"}'
           WHERE user_id = $1"#,
    )
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("corrupt privacy");

    let channel = deps
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm");
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, OWNER as u64).await,
        Err(SendRefusal::PolicyUnavailable),
        "脏数据必须拒绝：回落默认等于把用户的限制悄悄关掉",
    );

    cleanup(&pool).await;
}

/// 隐私设置必须**落库**：换一个全新 service（模拟重启/另一实例）仍然生效。
#[tokio::test]
async fn a_privacy_change_survives_a_fresh_service_instance() {
    use privchat::service::privacy_service::PrivacySettingsUpdate;

    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;

    deps.privacy_service
        .update_privacy_settings(
            MEMBER as u64,
            PrivacySettingsUpdate {
                allow_receive_message_from_non_friend: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("update privacy");

    // 全新实例：自带空缓存，只能从 DB 读。
    let (fresh, _) = deps_fresh().await.expect("fresh deps");
    let channel = fresh
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm");
    assert_eq!(
        authorize_send_to_channel(&fresh, &channel, OWNER as u64).await,
        Err(SendRefusal::PeerRejectsNonFriends),
        "只写缓存的话，重启或另一个实例上这条设置就没了",
    );

    cleanup(&pool).await;
}

async fn deps_fresh() -> Option<(SendAuthorizationDeps, Arc<sqlx::PgPool>)> {
    deps().await
}

/// 一个实例改了设置，另一个实例必须**立刻**看到。
///
/// ⚠️ 名实说明：隐私判定现在**直读数据库、完全不缓存**，所以这条在当前实现下
/// 是平凡成立的。它留在这里是**防回归**——谁要是为了省一次往返把缓存加回来，
/// 这条会立刻红，除非同时做了跨实例失效。
///
/// 🔴 也记下我在这条上犯过的错：早先的版本用 `CacheConfig::default()`
/// （`redis=None`）构造两个实例，号称「B 预热了旧缓存」——那里根本没有共享缓存，
/// 整条测试什么都没验证。要真验缓存一致性，必须连真 Redis。
#[tokio::test]
async fn an_updated_privacy_setting_reaches_an_instance_that_already_cached_the_old_one() {
    use privchat::service::privacy_service::PrivacySettingsUpdate;

    let _guard = fixture_lock().lock().await;
    let Some((instance_a, pool)) = deps().await else {
        return;
    };
    let (instance_b, _) = deps().await.expect("second instance");
    cleanup(&pool).await;
    ensure_user(&pool, OWNER, "sa_owner").await;
    ensure_user(&pool, MEMBER, "sa_member").await;
    seed_dm(&pool).await;

    let channel_b = instance_b
        .channel_service
        .get_channel_opt(DM_CHANNEL as u64)
        .await
        .expect("dm on b");

    // B 先判定一次，把「允许非好友消息」这个旧值读进它的缓存。
    assert!(
        authorize_send_to_channel(&instance_b, &channel_b, OWNER as u64)
            .await
            .is_ok(),
        "改之前是允许的",
    );

    // A 关掉它。
    instance_a
        .privacy_service
        .update_privacy_settings(
            MEMBER as u64,
            PrivacySettingsUpdate {
                allow_receive_message_from_non_friend: Some(false),
                ..Default::default()
            },
        )
        .await
        .expect("update on a");

    // B **立刻**必须拒绝，不能等 TTL。
    assert_eq!(
        authorize_send_to_channel(&instance_b, &channel_b, OWNER as u64).await,
        Err(SendRefusal::PeerRejectsNonFriends),
        "另一个实例的缓存必须被失效掉；靠 TTL 收敛意味着最长一小时内陌生人照发",
    );

    cleanup(&pool).await;
}

/// 并发改**不同字段**不得互相覆盖 —— 两个 update **同时** await。
///
/// 上一版两次 update 是顺序 await 的，只证明了 JSONB patch 不覆盖已有键，
/// 没有制造过任何真实交错。这里用 `tokio::join!` 让它们真的并发。
#[tokio::test]
async fn concurrent_updates_to_different_fields_do_not_overwrite_each_other() {
    use privchat::service::privacy_service::PrivacySettingsUpdate;

    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, MEMBER, "sa_member").await;

    // 🔴 确定性屏障：两个 update 在同一个点起跑。
    // 只用 `tokio::join!` 的话，谁先谁后取决于调度，退化成 read-modify-write 时
    // 未必每次都撞出丢更新窗口——那样这条测试会时灵时不灵。
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let a = deps.privacy_service.clone();
    let b = deps.privacy_service.clone();
    let (ba, bb) = (barrier.clone(), barrier.clone());
    let (first, second) = tokio::join!(
        async {
            ba.wait().await;
            a.update_privacy_settings(
                MEMBER as u64,
                PrivacySettingsUpdate {
                    allow_receive_message_from_non_friend: Some(false),
                    ..Default::default()
                },
            )
            .await
        },
        async {
            bb.wait().await;
            b.update_privacy_settings(
                MEMBER as u64,
                PrivacySettingsUpdate {
                    allow_search_by_phone: Some(false),
                    ..Default::default()
                },
            )
            .await
        },
    );
    first.expect("first update");
    second.expect("second update");

    // 无论谁先提交，两个字段都必须留在 DB 里。
    let (stored,): (serde_json::Value,) =
        sqlx::query_as("SELECT privacy_settings FROM privchat_users WHERE user_id = $1")
            .bind(MEMBER)
            .fetch_one(pool.as_ref())
            .await
            .expect("read back");
    assert_eq!(
        stored.get("allow_receive_message_from_non_friend"),
        Some(&serde_json::Value::Bool(false)),
        "并发交错下第一个字段不能丢",
    );
    assert_eq!(
        stored.get("allow_search_by_phone"),
        Some(&serde_json::Value::Bool(false)),
        "并发交错下第二个字段不能丢",
    );

    cleanup(&pool).await;
}

/// 【serde 边界回归】未知字段忽略；已知字段类型错误拒绝。
///
/// 两个方向都要钉住：只钉一边的话，下次有人为了「更严格」把
/// `deny_unknown_fields` 加回来，滚动升级期间老实例会把新版本写的字段
/// 判成脏数据、进而拒发消息。
#[tokio::test]
async fn unknown_fields_are_ignored_but_wrongly_typed_known_fields_are_refused() {
    let _guard = fixture_lock().lock().await;
    let Some((deps, pool)) = deps().await else {
        return;
    };
    cleanup(&pool).await;
    ensure_user(&pool, MEMBER, "sa_member").await;

    // 未来版本新增的字段：老实例必须照常工作。
    sqlx::query(
        r#"UPDATE privchat_users
           SET privacy_settings = '{"allow_receive_message_from_non_friend": false,
                                    "a_field_from_a_newer_version": true}'
           WHERE user_id = $1"#,
    )
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("write forward-compatible settings");

    let settings = deps
        .privacy_service
        .get_or_create_privacy_settings(MEMBER as u64)
        .await
        .expect("未知字段必须忽略，不能把整行判脏");
    assert!(
        !settings.allow_receive_message_from_non_friend,
        "认识的字段照常生效",
    );

    // 已知字段类型不对：脏数据，必须拒绝。
    sqlx::query(
        r#"UPDATE privchat_users
           SET privacy_settings = '{"allow_receive_message_from_non_friend": 42}'
           WHERE user_id = $1"#,
    )
    .bind(MEMBER)
    .execute(pool.as_ref())
    .await
    .expect("write corrupt settings");
    assert!(
        deps.privacy_service
            .get_or_create_privacy_settings(MEMBER as u64)
            .await
            .is_err(),
        "已知字段类型错误必须报错，回落默认等于把限制关掉",
    );

    cleanup(&pool).await;
}
