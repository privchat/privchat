// 设备会话在重新登录时的恢复规则。
//
// 线上事故：后台点了一次「强制下线全部设备」，此后该用户每次登录都是
// 「HTTP 登录成功、IM 连接被拒」。原因是 register_or_update_device 的 upsert
// 只 bump session_version、不碰 session_state，而 device_id 来自设备指纹、
// 每次登录都命中同一条记录 —— 状态一旦置为非 0 就再没有任何地方改回去。
// 那台设备的 session_version 涨到 17（登录十几次），state 始终是 3。
//
// 恢复与不恢复的分界不是「哪个状态更严重」，而是**解除条件是不是重新登录**：
//   Kicked(1)   被别的设备顶下线 —— 重登就是解除条件      → 恢复
//   Revoked(3)  定义即「必须重新登录」                     → 恢复
//   Frozen(2)   解除条件是风控放行                          → 不恢复
//   PendingVerify(4) 解除条件是二次验证                     → 不恢复
// 后两个若被重登顺手解掉，退出重进就能绕过风控。
//
// gate：未配 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL 时**不跳过而是失败**，
// 理由同 attachment_authz_db_test —— 跳过却记「通过」的绿灯证明不了 SQL 契约。
use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::auth::device_manager_db::DeviceManagerDb;
use privchat::auth::models::{Device, DeviceInfo, DeviceType};
use privchat::auth::SessionState;

// 独立 ID 段，避开真实数据与其它测试。
// 每个用例一套 id：cargo test 默认并行，共用同一条设备记录会互相改状态，
// 表现为「明明改对了却断言失败」，比没测更浪费时间。
const UID_RECOVER: u64 = 9_951_001;
const DEV_RECOVER: &str = "9a51e0de-0000-4000-8000-000000000001";
const UID_BUMP: u64 = 9_951_002;
const DEV_BUMP: &str = "9a51e0de-0000-4000-8000-000000000002";

async fn open_test_pool() -> Arc<sqlx::PgPool> {
    let url = privchat::require_test_database_url()
        .expect("需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL：跳过并记通过等于没测");
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .expect("测试库连接失败：连不上就该红，不能静默跳过");
    Arc::new(pool)
}

fn test_device(uid: u64, device_uuid: &str) -> Device {
    Device {
        device_id: device_uuid.to_string(),
        user_id: uid,
        business_system_id: "privchat-application".to_string(),
        device_info: DeviceInfo {
            app_id: "privchat".to_string(),
            device_name: "reactivate-test".to_string(),
            device_model: "test-model".to_string(),
            os_version: "14".to_string(),
            app_version: "1.0.29".to_string(),
        },
        device_type: DeviceType::Android,
        token_jti: "test-jti".to_string(),
        session_version: 1,
        session_state: SessionState::Active,
        kicked_at: None,
        kicked_reason: None,
        last_active_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        ip_address: "127.0.0.1".to_string(),
    }
}

async fn cleanup(pool: &sqlx::PgPool, uid: u64) {
    sqlx::query("DELETE FROM privchat_devices WHERE user_id = $1")
        .bind(uid as i64)
        .execute(pool)
        .await
        .expect("清理测试设备失败");
    sqlx::query("DELETE FROM privchat_users WHERE user_id = $1")
        .bind(uid as i64)
        .execute(pool)
        .await
        .expect("清理测试用户失败");
}

/// privchat_devices.user_id 有指向 privchat_users 的外键，设备得先有主人。
async fn ensure_user(pool: &sqlx::PgPool, uid: u64) {
    sqlx::query(
        "INSERT INTO privchat_users (user_id, qr_key) VALUES ($1, $2) ON CONFLICT (user_id) DO NOTHING",
    )
    .bind(uid as i64)
    .bind(format!("qr-{uid}"))
    .execute(pool)
    .await
    .expect("建测试用户失败");
}

/// 把设备置为指定状态，并写上踢出痕迹（模拟真实的踢出/撤销）。
async fn force_state(pool: &sqlx::PgPool, uid: u64, device_uuid: &str, state: i16) {
    sqlx::query(
        r#"
        UPDATE privchat_devices
        SET session_state = $1,
            kicked_at = 1700000000000,
            kicked_reason = 'test forced',
            updated_at = 1700000000000
        WHERE user_id = $2 AND device_id = $3
        "#,
    )
    .bind(state)
    .bind(uid as i64)
    .bind(uuid::Uuid::parse_str(device_uuid).unwrap())
    .execute(pool)
    .await
    .expect("置状态失败");
}

async fn read_state(pool: &sqlx::PgPool, uid: u64, device_uuid: &str) -> (i16, i64, Option<String>) {
    let row: (i16, i64, Option<String>) = sqlx::query_as(
        "SELECT session_state, session_version, kicked_reason FROM privchat_devices \
         WHERE user_id = $1 AND device_id = $2",
    )
    .bind(uid as i64)
    .bind(uuid::Uuid::parse_str(device_uuid).unwrap())
    .fetch_one(pool)
    .await
    .expect("读设备失败");
    row
}

#[tokio::test]
async fn relogin_recovers_kicked_and_revoked_but_not_frozen() {
    let pool = open_test_pool().await;
    let manager = DeviceManagerDb::new(pool.clone());
    let (uid, dev) = (UID_RECOVER, DEV_RECOVER);
    cleanup(&pool, uid).await;
    ensure_user(&pool, uid).await;

    let device = test_device(uid, dev);

    // 首次注册：新设备应当是 Active
    let (created, _) = manager
        .register_or_update_device(&device)
        .await
        .expect("首次注册失败");
    assert!(created, "第一次应当是新建");
    let (state, _, _) = read_state(&pool, uid, dev).await;
    assert_eq!(state, 0, "新设备应为 Active");

    // Kicked(1) → 重新登录应恢复，并清掉踢出痕迹
    force_state(&pool, uid, dev, 1).await;
    manager
        .register_or_update_device(&device)
        .await
        .expect("重登失败");
    let (state, _, reason) = read_state(&pool, uid, dev).await;
    assert_eq!(state, 0, "被顶下线的设备重新登录后应恢复");
    assert!(reason.is_none(), "恢复后不该残留 kicked_reason：{reason:?}");

    // Revoked(3) → 同上。这就是线上那台设备的处境
    force_state(&pool, uid, dev, 3).await;
    manager
        .register_or_update_device(&device)
        .await
        .expect("重登失败");
    let (state, _, reason) = read_state(&pool, uid, dev).await;
    assert_eq!(state, 0, "被撤销的设备重新登录后应恢复，否则永久报废");
    assert!(reason.is_none(), "恢复后不该残留 kicked_reason：{reason:?}");

    // Frozen(2) → 重新登录**不**恢复，否则退出重进即可绕过风控
    force_state(&pool, uid, dev, 2).await;
    manager
        .register_or_update_device(&device)
        .await
        .expect("重登失败");
    let (state, _, _) = read_state(&pool, uid, dev).await;
    assert_eq!(state, 2, "风控冻结不能被一次重新登录解掉");

    // PendingVerify(4) → 同样保持，解除条件是二次验证
    force_state(&pool, uid, dev, 4).await;
    manager
        .register_or_update_device(&device)
        .await
        .expect("重登失败");
    let (state, _, _) = read_state(&pool, uid, dev).await;
    assert_eq!(state, 4, "待验证状态不能被一次重新登录解掉");

    cleanup(&pool, uid).await;
}

#[tokio::test]
async fn relogin_still_bumps_session_version_when_state_is_kept() {
    // 恢复与否不影响「重新登录作废上一次会话」这条既有约定：
    // 即便状态保持 Frozen，session_version 也必须继续 +1，
    // 否则丢一次登出通知就等于旧 token 永久续命。
    let pool = open_test_pool().await;
    let manager = DeviceManagerDb::new(pool.clone());
    let (uid, dev) = (UID_BUMP, DEV_BUMP);
    cleanup(&pool, uid).await;
    ensure_user(&pool, uid).await;

    let device = test_device(uid, dev);
    manager
        .register_or_update_device(&device)
        .await
        .expect("首次注册失败");

    force_state(&pool, uid, dev, 2).await;
    let (_, before, _) = read_state(&pool, uid, dev).await;
    manager
        .register_or_update_device(&device)
        .await
        .expect("重登失败");
    let (state, after, _) = read_state(&pool, uid, dev).await;

    assert_eq!(state, 2, "状态应保持 Frozen");
    assert_eq!(after, before + 1, "session_version 仍应 +1");

    cleanup(&pool, uid).await;
}
