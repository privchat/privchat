// 群角色变更的授权门禁。
//
// 🔴 这两条路由此前是**提权链**：
//   - `group/role/set` 一处权限校验都没有——`operator_id` 从请求体读出来后再没被用过，
//     任何登录用户都能把任意人（包括自己）设成任意群的管理员；
//   - `group/role/transfer_owner` 整段不看认证上下文，直接信请求体里的
//     `current_owner_id`，填上真群主的 ID 就能把群转给自己。
//
// 而 `group/settings/update` 的「只有群主可以改」正是读这份角色，于是绕过它只要两步。
//
// 这里打真库，验的是**服务层**的判定与落库（handler 的认证取值另有单元测试）。

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::model::channel::MemberRole;
use privchat::repository::PgChannelRepository;
use privchat::service::ChannelService;

const GROUP_ID: i64 = 9_920_001;
const OWNER: i64 = 9_920_101;
const ADMIN: i64 = 9_920_102;
const MEMBER: i64 = 9_920_103;

fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn service() -> Option<(Arc<ChannelService>, Arc<sqlx::PgPool>)> {
    let url = privchat::require_test_database_url()?;
    let pool = Arc::new(
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .unwrap_or_else(|e| panic!("连接测试数据库失败（{url}）: {e}")),
    );
    let service = Arc::new(ChannelService::new_with_repository(Arc::new(
        PgChannelRepository::new(pool.clone()),
    )));
    Some((service, pool))
}

async fn seed(pool: &sqlx::PgPool) {
    cleanup(pool).await;
    for (uid, name) in [(OWNER, "role_owner"), (ADMIN, "role_admin"), (MEMBER, "role_member")] {
        sqlx::query(
            "INSERT INTO privchat_users (user_id, username, display_name, qr_key) \
             VALUES ($1, $2, $2, $3) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(uid)
        .bind(name)
        .bind(privchat::rpc::qr::generate_qr_key())
        .execute(pool)
        .await
        .expect("user");
    }
    sqlx::query(
        "INSERT INTO privchat_groups (group_id, name, owner_id, member_count, qr_key) \
         VALUES ($1, 'role-authz', $2, 3, $3)",
    )
    .bind(GROUP_ID)
    .bind(OWNER)
    .bind(privchat::rpc::qr::generate_qr_key())
    .execute(pool)
    .await
    .expect("group");
    // Owner=0 / Admin=1 / Member=2
    for (uid, role) in [(OWNER, 0i16), (ADMIN, 1i16), (MEMBER, 2i16)] {
        sqlx::query(
            "INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, updated_at) \
             VALUES ($1, $2, $3, now_millis(), now_millis())",
        )
        .bind(GROUP_ID)
        .bind(uid)
        .bind(role)
        .execute(pool)
        .await
        .expect("member");
    }
}

async fn cleanup(pool: &sqlx::PgPool) {
    sqlx::query("DELETE FROM privchat_group_members WHERE group_id = $1")
        .bind(GROUP_ID)
        .execute(pool)
        .await
        .expect("clean members");
    sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
        .bind(GROUP_ID)
        .execute(pool)
        .await
        .expect("clean group");
}

async fn role_of(pool: &sqlx::PgPool, user_id: i64) -> Option<i16> {
    sqlx::query_scalar(
        "SELECT role FROM privchat_group_members \
         WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL",
    )
    .bind(GROUP_ID)
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .expect("read role")
}

async fn owner_of(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("SELECT owner_id FROM privchat_groups WHERE group_id = $1")
        .bind(GROUP_ID)
        .fetch_one(pool)
        .await
        .expect("read owner")
}

/// 一次群设置更新只能产生**一次**落库。
///
/// 🔴 `all_muted` 曾被写两遍：`update_group_policy` 一次、`set_channel_all_muted`
/// 内部又一次。群实体上挂着 sync_version trigger，多写一次等于把相关用户
/// 再推一轮增量同步——功能看不出差别，账单和带宽看得出。
#[tokio::test]
async fn muting_everyone_writes_the_group_row_once() {
    let _guard = fixture_lock().lock().await;
    let Some((service, pool)) = service().await else {
        return;
    };
    seed(&pool).await;

    let before: i64 = sqlx::query_scalar("SELECT updated_at FROM privchat_groups WHERE group_id = $1")
        .bind(GROUP_ID)
        .fetch_one(pool.as_ref())
        .await
        .expect("read updated_at");

    service
        .update_group_policy(GROUP_ID as u64, None, None, None, None, Some(true), None, None)
        .await
        .expect("落库 all_muted");
    let after_policy: i64 =
        sqlx::query_scalar("SELECT updated_at FROM privchat_groups WHERE group_id = $1")
            .bind(GROUP_ID)
            .fetch_one(pool.as_ref())
            .await
            .expect("read updated_at");
    assert!(after_policy > before, "策略落库应该更新了这一行");

    // handler 随后只刷缓存，不得再写库。
    service.sync_all_muted_cache(&(GROUP_ID as u64), true).await;
    let after_cache: i64 =
        sqlx::query_scalar("SELECT updated_at FROM privchat_groups WHERE group_id = $1")
            .bind(GROUP_ID)
            .fetch_one(pool.as_ref())
            .await
            .expect("read updated_at");
    assert_eq!(
        after_cache, after_policy,
        "缓存同步不能再碰数据库——再写一次就是第二次 sync_version 推进",
    );

    cleanup(&pool).await;
}

/// 冒充群主的转让必须被拒——判定在事务内按 DB 复核，不看调用方自报的身份。
#[tokio::test]
async fn a_member_cannot_transfer_ownership_by_naming_the_real_owner() {
    let _guard = fixture_lock().lock().await;
    let Some((service, pool)) = service().await else {
        return;
    };
    seed(&pool).await;

    // ⚠️ 这里**不能**用「转给自己」来测：那会先撞上「不能转让给自己」的早退分支，
    // 于是即使把 owner 复核整段删掉测试也照样绿——我第一版就是这么写的，白测一轮。
    // 攻击形态本来也不是转给自己：旧代码是「请求体里写真群主的 ID」来通过判定。

    // 普通成员发起转让（转给管理员，避开自转早退）。
    let refused = service
        .transfer_group_owner(GROUP_ID as u64, MEMBER as u64, ADMIN as u64)
        .await;
    assert!(refused.is_err(), "非群主发起的转让必须被拒");

    // 管理员发起也不行。
    let refused = service
        .transfer_group_owner(GROUP_ID as u64, ADMIN as u64, MEMBER as u64)
        .await;
    assert!(refused.is_err(), "管理员也不能替群主转让");

    assert_eq!(owner_of(&pool).await, OWNER, "群主没有易主");
    assert_eq!(role_of(&pool, MEMBER).await, Some(2), "普通成员角色没变");
    assert_eq!(role_of(&pool, ADMIN).await, Some(1), "管理员角色没变");

    cleanup(&pool).await;
}

/// 真群主转让：成员表与 `privchat_groups.owner_id` 必须一起变。
///
/// owner_id 是第二处真源，只改成员表会得到「成员表说是他、群表说是你」的分裂状态。
#[tokio::test]
async fn a_real_transfer_moves_both_the_membership_row_and_the_group_owner() {
    let _guard = fixture_lock().lock().await;
    let Some((service, pool)) = service().await else {
        return;
    };
    seed(&pool).await;

    service
        .transfer_group_owner(GROUP_ID as u64, OWNER as u64, ADMIN as u64)
        .await
        .expect("群主本人转让");

    assert_eq!(role_of(&pool, ADMIN).await, Some(0), "新群主是 Owner");
    assert_eq!(role_of(&pool, OWNER).await, Some(2), "旧群主降为普通成员");
    assert_eq!(owner_of(&pool).await, ADMIN, "privchat_groups.owner_id 跟着走");

    cleanup(&pool).await;
}

/// 转让给非本群成员必须被拒，且不留下半个状态。
#[tokio::test]
async fn transferring_to_a_non_member_changes_nothing() {
    let _guard = fixture_lock().lock().await;
    let Some((service, pool)) = service().await else {
        return;
    };
    seed(&pool).await;

    let refused = service
        .transfer_group_owner(GROUP_ID as u64, OWNER as u64, 9_920_999)
        .await;
    assert!(refused.is_err(), "新群主必须是本群成员");
    assert_eq!(owner_of(&pool).await, OWNER, "群主没变");
    assert_eq!(role_of(&pool, OWNER).await, Some(0), "旧群主没有被先降级");

    cleanup(&pool).await;
}

/// 设角色走落库版本：不能把人设成 Owner，也不能改动现任 Owner。
///
/// 原 handler 用的是只改内存的 setter，角色重启即回退，而鉴权在别处读 DB 真源。
#[tokio::test]
async fn setting_roles_persists_and_refuses_to_touch_ownership() {
    let _guard = fixture_lock().lock().await;
    let Some((service, pool)) = service().await else {
        return;
    };
    seed(&pool).await;

    service
        .set_member_role_admin(GROUP_ID as u64, MEMBER as u64, MemberRole::Admin)
        .await
        .expect("提管理员");
    assert_eq!(role_of(&pool, MEMBER).await, Some(1), "角色必须落库");

    assert!(
        service
            .set_member_role_admin(GROUP_ID as u64, MEMBER as u64, MemberRole::Owner)
            .await
            .is_err(),
        "不能借这个接口造出第二个群主",
    );
    assert!(
        service
            .set_member_role_admin(GROUP_ID as u64, OWNER as u64, MemberRole::Member)
            .await
            .is_err(),
        "不能借这个接口把现任群主降级",
    );

    assert_eq!(owner_of(&pool).await, OWNER);
    assert_eq!(role_of(&pool, OWNER).await, Some(0));

    cleanup(&pool).await;
}
