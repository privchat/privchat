// 服务端撤销设备必须幂等：module-agent 撤节点后会持久重试，直到确认。
// 之前第二次调用返回 NotFound 且跳过断连 —— 首次调用若在改库后、断连前中断，
// 重试永远报错，节点连接也一直不断。
//
// gate：未配 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL 时失败，不跳过。
use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use privchat::auth::device_manager_db::DeviceManagerDb;
use privchat::auth::models::{Device, DeviceInfo, DeviceType};
use privchat::auth::SessionState;
use privchat::infra::ConnectionManager;
use privchat::repository::{PgChannelRepository, PgMessageRepository, UserRepository};
use privchat::service::{AdminService, ChannelService};

const UID: u64 = 9_952_001;
const DEV: &str = "9a52e0de-0000-4000-8000-000000000001";

#[tokio::test]
async fn revoking_an_already_revoked_device_succeeds() {
    let url = privchat::require_test_database_url()
        .expect("需要 PRIVCHAT_TEST_DATABASE_URL / DATABASE_URL：跳过并记通过等于没测");
    let pool = Arc::new(PgPoolOptions::new().max_connections(4).connect(&url).await.expect("测试库连接失败"));
    for sql in ["DELETE FROM privchat_devices WHERE user_id = $1", "DELETE FROM privchat_users WHERE user_id = $1"] {
        sqlx::query(sql).bind(UID as i64).execute(&*pool).await.unwrap();
    }
    sqlx::query("INSERT INTO privchat_users (user_id, qr_key) VALUES ($1, $2)")
        .bind(UID as i64).bind(format!("qr-{UID}")).execute(&*pool).await.unwrap();
    let devices = Arc::new(DeviceManagerDb::new(pool.clone()));
    devices
        .register_or_update_device(&Device {
            device_id: DEV.to_string(),
            user_id: UID,
            business_system_id: "privchat-application".to_string(),
            device_info: DeviceInfo {
                app_id: "weey-agent".to_string(),
                device_name: "node".to_string(),
                device_model: "test".to_string(),
                os_version: "1".to_string(),
                app_version: "0.1.0".to_string(),
            },
            device_type: DeviceType::Android,
            token_jti: "jti".to_string(),
            session_version: 1,
            session_state: SessionState::Active,
            kicked_at: None,
            kicked_reason: None,
            last_active_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            ip_address: "127.0.0.1".to_string(),
        })
        .await
        .unwrap();
    let admin = AdminService::new(
        Arc::new(UserRepository::new(pool.clone())),
        devices.clone(),
        Arc::new(ConnectionManager::new()),
        Arc::new(ChannelService::new_with_repository(Arc::new(PgChannelRepository::new(pool.clone())))),
        Arc::new(PgMessageRepository::new(pool.clone())),
    );

    admin.revoke_device(UID, DEV, "agent_node_revoked").await.expect("first revoke");
    let version = || async {
        sqlx::query_as::<_, (i16, i64)>("SELECT session_state, session_version FROM privchat_devices WHERE user_id=$1")
            .bind(UID as i64).fetch_one(&*pool).await.unwrap()
    };
    let after_first = version().await;
    assert_eq!(after_first.0, 1, "device is kicked");
    admin.revoke_device(UID, DEV, "agent_node_revoked").await.expect("retry is idempotent");
    assert_eq!(version().await, after_first, "retry does not bump the session again");
    admin
        .revoke_device(UID, "9a52e0de-0000-4000-8000-0000000000ff", "agent_node_revoked")
        .await
        .expect("an unknown device is already in the revoked end state");
    // 非法 device id 仍是请求错误，不被吞掉。
    assert!(admin.revoke_device(UID, "not-a-uuid", "x").await.is_err());

    for sql in ["DELETE FROM privchat_devices WHERE user_id = $1", "DELETE FROM privchat_users WHERE user_id = $1"] {
        sqlx::query(sql).bind(UID as i64).execute(&*pool).await.unwrap();
    }
}
