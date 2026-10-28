use super::*;
use privchat_protocol::rpc::group::group::GroupProfilePatch;

#[test]
fn rename_payload_uses_shared_system_template_and_typed_refs() {
    let payload = rename_system_payload(42, "Operator", "Renamed group");
    assert_eq!(payload["message_type"], "system");
    assert_eq!(payload["template"], "system.group_name_changed");
    assert_eq!(payload["refs"].as_array().unwrap().len(), 2);
    assert_eq!(
        payload["refs"][0],
        serde_json::json!({
            "type": "user", "target_id": "42", "text": "Operator"
        })
    );
    assert_eq!(
        payload["refs"][1],
        serde_json::json!({
            "type": "text", "text": "Renamed group"
        })
    );
}

async fn seed(group: i64) -> sqlx::PgPool {
    let url =
        crate::require_test_database_url().expect("explicit disposable TEST_DATABASE_URL required");
    let pool = sqlx::PgPool::connect(&url)
        .await
        .expect("real PostgreSQL connection");
    cleanup(&pool, group).await;
    sqlx::query("INSERT INTO privchat_users(user_id,username,qr_key) VALUES(1,'system','rename-system') ON CONFLICT(user_id) DO NOTHING").execute(&pool).await.unwrap();
    for user in [group + 1, group + 2, group + 3] {
        sqlx::query("INSERT INTO privchat_users (user_id, username, qr_key) VALUES ($1,$2,$2)")
            .bind(user)
            .bind(format!("rn{user}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query(
        "INSERT INTO privchat_groups (group_id,name,owner_id,qr_key) VALUES ($1,'Original',$2,$3)",
    )
    .bind(group)
    .bind(group + 1)
    .bind(format!("rg{group}"))
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO privchat_channels(channel_id,channel_type,group_id) VALUES($1,1,$1)")
        .bind(group)
        .execute(&pool)
        .await
        .unwrap();
    // Database role encoding differs from the public protocol encoding.
    for (user, role) in [(group + 1, 0i16), (group + 2, 1i16), (group + 3, 2i16)] {
        sqlx::query("INSERT INTO privchat_group_members (group_id,user_id,role,joined_at,updated_at) VALUES ($1,$2,$3,now_millis(),now_millis())")
            .bind(group).bind(user).bind(role).execute(&pool).await.unwrap();
    }
    pool
}

fn patch(group: i64, name: Option<&str>, description: Option<&str>) -> GroupUpdateRequest {
    GroupUpdateRequest {
        group_id: group as u64,
        profile: GroupProfilePatch {
            name: name.map(str::to_owned),
            description: description.map(str::to_owned),
        },
    }
}

async fn cleanup(pool: &sqlx::PgPool, group: i64) {
    for statement in [
        "DELETE FROM privchat_message_dispatch_recipient WHERE event_id IN (SELECT id FROM privchat_commit_log WHERE channel_id=$1)",
        "DELETE FROM privchat_message_dispatch_outbox WHERE channel_id=$1",
        "DELETE FROM privchat_commit_log WHERE channel_id=$1",
        "DELETE FROM privchat_messages WHERE channel_id=$1",
        "DELETE FROM privchat_channel_pts WHERE channel_id=$1",
        "DELETE FROM privchat_channels WHERE channel_id=$1",
    ] {
        sqlx::query(statement).bind(group).execute(pool).await.unwrap();
    }
    sqlx::query("DELETE FROM privchat_group_members WHERE group_id=$1")
        .bind(group)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM privchat_groups WHERE group_id=$1")
        .bind(group)
        .execute(pool)
        .await
        .unwrap();
    for user in [group + 1, group + 2, group + 3] {
        sqlx::query("DELETE FROM privchat_users WHERE user_id=$1")
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn owner_and_admin_rename_marker_matches_committed_normalized_name() {
    let group = 9_941_100;
    let pool = seed(group).await;
    for (actor, name) in [(group + 1, " Owner name "), (group + 2, " Admin name ")] {
        let (response, changed, renamed) =
            update_profile_with_rename(&pool, actor as u64, patch(group, Some(name), None))
                .await
                .unwrap();
        assert!(changed && renamed);
        assert_eq!(response.name, name.trim());
        let stored: String =
            sqlx::query_scalar("SELECT name FROM privchat_groups WHERE group_id=$1")
                .bind(group)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, response.name);
        let content: String = sqlx::query_scalar(
            "SELECT content FROM privchat_messages WHERE channel_id=$1 ORDER BY pts DESC LIMIT 1",
        )
        .bind(group)
        .fetch_one(&pool)
        .await
        .unwrap();
        let payload: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(payload["template"], "system.group_name_changed");
        assert_eq!(payload["refs"][0]["target_id"], actor.to_string());
        assert_eq!(payload["refs"][1]["text"], response.name);
    }
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn no_op_and_description_only_do_not_schedule_rename_messages() {
    let group = 9_941_200;
    let pool = seed(group).await;
    let (before, changed, renamed) =
        update_profile_with_rename(&pool, (group + 1) as u64, patch(group, None, None))
            .await
            .unwrap();
    assert!(!changed && !renamed);
    let (same, changed, renamed) = update_profile_with_rename(
        &pool,
        (group + 2) as u64,
        patch(group, Some(" Original "), None),
    )
    .await
    .unwrap();
    assert!(!changed && !renamed);
    assert_eq!(same.updated_at, before.updated_at);
    assert_eq!(same.sync_version, before.sync_version);
    let (description, changed, renamed) = update_profile_with_rename(
        &pool,
        (group + 2) as u64,
        patch(group, None, Some("Introduction")),
    )
    .await
    .unwrap();
    assert!(changed && !renamed);
    assert_eq!(description.description.as_deref(), Some("Introduction"));
    assert_eq!(timeline_counts(&pool, group).await, (0, 0, 0, 0));
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn member_and_invalid_name_rejections_do_not_mutate_profile() {
    let group = 9_941_300;
    let pool = seed(group).await;
    assert!(update_profile_with_rename(
        &pool,
        (group + 3) as u64,
        patch(group, Some("Forbidden"), None)
    )
    .await
    .is_err());
    assert!(update_profile_with_rename(
        &pool,
        (group + 1) as u64,
        patch(group, Some("bad\nname"), None)
    )
    .await
    .is_err());
    let stored: String = sqlx::query_scalar("SELECT name FROM privchat_groups WHERE group_id=$1")
        .bind(group)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stored, "Original");
    cleanup(&pool, group).await;
}

async fn timeline_counts(pool: &sqlx::PgPool, group: i64) -> (i64, i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM privchat_messages WHERE channel_id=$1), (SELECT COUNT(*) FROM privchat_commit_log WHERE channel_id=$1), (SELECT COUNT(*) FROM privchat_message_dispatch_outbox WHERE channel_id=$1), (SELECT COUNT(*) FROM privchat_message_dispatch_recipient WHERE event_id IN (SELECT id FROM privchat_commit_log WHERE channel_id=$1))")
        .bind(group).fetch_one(pool).await.unwrap()
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn outbox_failure_rolls_back_profile_message_pts_and_head_then_retry_succeeds() {
    let group = 9_941_400;
    let pool = seed(group).await;
    let before: (String, i64, i64) = sqlx::query_as(
        "SELECT name,updated_at,sync_version FROM privchat_groups WHERE group_id=$1",
    )
    .bind(group)
    .fetch_one(&pool)
    .await
    .unwrap();
    // Real late SQL failure, after message/pts/head/commit-log writes, on this fixture DB only.
    sqlx::raw_sql("CREATE OR REPLACE FUNCTION rename_test_outbox_failure() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.channel_id=9941400 THEN RAISE EXCEPTION 'rename test injected outbox failure'; END IF; RETURN NEW; END $$; DROP TRIGGER IF EXISTS rename_test_fail ON privchat_message_dispatch_outbox; CREATE TRIGGER rename_test_fail BEFORE INSERT ON privchat_message_dispatch_outbox FOR EACH ROW EXECUTE FUNCTION rename_test_outbox_failure();").execute(&pool).await.unwrap();
    let result = update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Atomic rename"), None),
    )
    .await;
    sqlx::raw_sql("DROP TRIGGER rename_test_fail ON privchat_message_dispatch_outbox; DROP FUNCTION rename_test_outbox_failure();").execute(&pool).await.unwrap();
    assert!(result.is_err());
    let after: (String, i64, i64) = sqlx::query_as(
        "SELECT name,updated_at,sync_version FROM privchat_groups WHERE group_id=$1",
    )
    .bind(group)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(before, after);
    assert_eq!(timeline_counts(&pool, group).await, (0, 0, 0, 0));
    let pts: Option<i64> =
        sqlx::query_scalar("SELECT current_pts FROM privchat_channel_pts WHERE channel_id=$1")
            .bind(group)
            .fetch_optional(&pool)
            .await
            .unwrap();
    assert_eq!(pts, None);
    let head: (Option<i64>,Option<i64>) = sqlx::query_as("SELECT server_latest_message_pts,server_latest_message_id FROM privchat_channels WHERE channel_id=$1").bind(group).fetch_one(&pool).await.unwrap();
    assert_eq!(head, (None, None));
    assert!(
        update_profile(
            &pool,
            (group + 1) as u64,
            patch(group, Some("Atomic rename"), None)
        )
        .await
        .unwrap()
        .1
    );
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn caller_rollback_discards_notice_and_profile_then_retry_commits_once() {
    let group = 9_941_500;
    let pool = seed(group).await;
    let repo = crate::repository::PgMessageRepository::new(std::sync::Arc::new(pool.clone()));
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("UPDATE privchat_groups SET name='Aborted rename' WHERE group_id=$1")
        .bind(group)
        .execute(&mut *tx)
        .await
        .unwrap();
    crate::service::message_service::MessageService::persist_group_system_message_in_transaction(
        &repo,
        &mut tx,
        group as u64,
        rename_system_payload((group + 1) as u64, "Operator", "Aborted rename").to_string(),
    )
    .await
    .unwrap();
    // The message writer has released its savepoint, but must not commit the caller.
    tx.rollback().await.unwrap();
    assert_eq!(timeline_counts(&pool, group).await, (0, 0, 0, 0));
    let name: String = sqlx::query_scalar("SELECT name FROM privchat_groups WHERE group_id=$1")
        .bind(group)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Original");
    let first = update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Aborted rename"), None),
    )
    .await
    .unwrap();
    let retry = update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Aborted rename"), None),
    )
    .await
    .unwrap();
    assert!(first.1 && !retry.1);
    assert_eq!(first.0.sync_version, retry.0.sync_version);
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    // Returning to an earlier name is a new mutation, not an idempotency collision.
    update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Original"), None),
    )
    .await
    .unwrap();
    update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Aborted rename"), None),
    )
    .await
    .unwrap();
    assert_eq!(timeline_counts(&pool, group).await, (3, 3, 3, 9));
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn concurrent_owner_admin_same_name_has_one_notice_and_noop_retry() {
    let group = 9_941_600;
    let pool = seed(group).await;
    let a = update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Concurrent"), None),
    );
    let b = update_profile(
        &pool,
        (group + 2) as u64,
        patch(group, Some("Concurrent"), None),
    );
    let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(a, b)
    })
    .await
    .expect("no lock-order deadlock");
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a.1, b.1);
    assert_eq!(a.0.sync_version, b.0.sync_version);
    assert_eq!(a.0.updated_at, b.0.updated_at);
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    assert!(
        !update_profile(
            &pool,
            (group + 1) as u64,
            patch(group, Some("Concurrent"), None)
        )
        .await
        .unwrap()
        .1
    );
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL and Redis fixtures; never a no-env pass"]
async fn recreated_worker_recovers_committed_rename_after_expired_claim() {
    use crate::service::{
        CommittedTimelineDeliveryService, DispatchOutboxStore, OfflineQueueService,
        RecipientDeliveryOutcome,
    };
    use std::sync::Arc;
    let group = 9_941_700;
    let pool = Arc::new(seed(group).await);
    let redis = Arc::new(
        crate::infra::redis::RedisClient::new(&crate::config::RedisConfig {
            url: std::env::var("PRIVCHAT_TEST_REDIS_URL")
                .expect("explicit isolated Redis URL required"),
            pool_size: 2,
            min_idle: 1,
            connection_timeout_secs: 3,
            command_timeout_ms: 3000,
            idle_timeout_secs: 30,
        })
        .await
        .unwrap(),
    );
    let offline = Arc::new(OfflineQueueService::new(redis));
    for user in group + 1..=group + 3 {
        offline.clear(user as u64).await.unwrap();
    }
    update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Recovered rename"), None),
    )
    .await
    .unwrap();
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    // Claim and abandon using the real store, as a worker exiting before handoff would.
    let store = DispatchOutboxStore::new(pool.clone());
    let claims = store
        .claim_pending("rename-exited-worker", 100)
        .await
        .unwrap();
    assert_eq!(claims.len(), 3);
    assert!(claims.iter().all(|c| c.channel_id == group));
    drop(store);
    sqlx::query("UPDATE privchat_message_dispatch_recipient SET lease_until=0 WHERE event_id IN (SELECT id FROM privchat_commit_log WHERE channel_id=$1)").bind(group).execute(pool.as_ref()).await.unwrap();
    let repo = Arc::new(crate::repository::PgMessageRepository::new(pool.clone()));
    let router = Arc::new(crate::infra::MessageRouter::new(
        crate::infra::message_router::MessageRouterConfig::default(),
        Arc::new(crate::infra::ConnectionManager::new()),
    ));
    let worker = CommittedTimelineDeliveryService::new(
        pool.clone(),
        router,
        repo,
        offline.clone(),
        Arc::new(tokio::sync::Semaphore::new(4)),
        "rename-recovery-worker",
    );
    let outcomes = worker.process_once().await.unwrap();
    assert_eq!(outcomes.len(), 3);
    assert!(outcomes
        .iter()
        .all(|o| matches!(o.outcome, RecipientDeliveryOutcome::OfflineQueued)));
    for user in group + 1..=group + 3 {
        let queued = offline.get_all(user as u64).await.unwrap();
        assert_eq!(queued.len(), 1);
        let envelope: Value = serde_json::from_slice(&queued[0].payload).unwrap();
        let notice: Value = serde_json::from_str(envelope["content"].as_str().unwrap()).unwrap();
        assert_eq!(notice["template"], "system.group_name_changed");
        assert_eq!(notice["refs"][1]["text"], "Recovered rename");
        offline.clear(user as u64).await.unwrap();
    }
    assert!(worker.process_once().await.unwrap().is_empty());
    let terminal: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM privchat_message_dispatch_outbox WHERE channel_id=$1 AND status=1",
    )
    .bind(group)
    .fetch_one(pool.as_ref())
    .await
    .unwrap();
    assert_eq!(terminal, 1);
    assert!(
        !update_profile(
            &pool,
            (group + 1) as u64,
            patch(group, Some("Recovered rename"), None)
        )
        .await
        .unwrap()
        .1
    );
    assert_eq!(timeline_counts(&pool, group).await, (1, 1, 1, 3));
    cleanup(&pool, group).await;
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL fixture; never a no-env pass"]
async fn one_connection_registry_duplicate_rolls_back_only_message_savepoint() {
    use crate::repository::{
        AtomicMessageCommitRequest, ClientRegistryClaim, MessageRepository, PgMessageRepository,
    };
    use privchat_protocol::{CanonicalTimelineEvent, MessagePayloadEnvelope, NewMessageEvent};
    use std::sync::Arc;
    let group = 9_941_800;
    let pool = seed(group).await;
    update_profile(
        &pool,
        (group + 1) as u64,
        patch(group, Some("Registry seed"), None),
    )
    .await
    .unwrap();
    let id: i64 =
        sqlx::query_scalar("SELECT message_id FROM privchat_messages WHERE channel_id=$1")
            .bind(group)
            .fetch_one(&pool)
            .await
            .unwrap();
    let source = PgMessageRepository::new(Arc::new(pool.clone()));
    let mut message = source.find_by_id(id as u64).await.unwrap().unwrap();
    message.message_id = crate::infra::next_message_id();
    message.local_message_id = Some(71018);
    message.pts = None;
    let single = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&crate::require_test_database_url().unwrap())
        .await
        .unwrap();
    let repo = PgMessageRepository::new(Arc::new(single.clone()));
    let mut request = AtomicMessageCommitRequest {
        event: CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: message.message_type,
            payload: MessagePayloadEnvelope {
                content: message.content.clone(),
                ..Default::default()
            },
        }),
        message,
        dedup_key: None,
        client_registry_claim: Some(ClientRegistryClaim {
            device_id: "rename-single".into(),
            decision: "accepted".into(),
        }),
        attachment_refs: Vec::new(),
        channel_type: 2,
        sender_username: None,
    };
    let first = repo
        .create_message_and_commit_atomic(request.clone())
        .await
        .unwrap();
    assert!(first.inserted);
    request.message.message_id = crate::infra::next_message_id();
    let mut tx = single.begin().await.unwrap();
    sqlx::query("UPDATE privchat_groups SET description='caller preserved' WHERE group_id=$1")
        .bind(group)
        .execute(&mut *tx)
        .await
        .unwrap();
    let duplicate = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        repo.create_message_in_transaction(&mut tx, request),
    )
    .await
    .expect("duplicate must not acquire a second pool connection")
    .unwrap();
    assert!(!duplicate.inserted);
    assert_eq!(duplicate.message.message_id, first.message.message_id);
    tx.commit().await.unwrap();
    let description: String =
        sqlx::query_scalar("SELECT description FROM privchat_groups WHERE group_id=$1")
            .bind(group)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(description, "caller preserved");
    assert_eq!(timeline_counts(&pool, group).await, (2, 2, 2, 6));
    sqlx::query("DELETE FROM privchat_client_msg_registry WHERE channel_id=$1")
        .bind(group)
        .execute(&pool)
        .await
        .unwrap();
    cleanup(&pool, group).await;
}
