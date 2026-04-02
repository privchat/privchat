use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use privchat::infra::{DeviceId, MessageRouter, MessageRouterConfig, SessionId, UserId};
use privchat::infra::cache::{L1L2Cache, TwoLevelCache};
use privchat::infra::message_router::{OfflineMessage, SessionManager as RouterSessionManager, UserOnlineStatus};
use privchat_protocol::protocol::PushMessageRequest;
use std::time::Duration;
use tokio::sync::RwLock;

#[derive(Default)]
struct TestSessionManager {
    online_sessions: RwLock<HashSet<SessionId>>,
    delivered: RwLock<HashMap<SessionId, Vec<PushMessageRequest>>>,
    batch_delivered: RwLock<HashMap<SessionId, Vec<Vec<PushMessageRequest>>>>,
}

impl TestSessionManager {
    async fn set_online(&self, session_id: &str) {
        self.online_sessions.write().await.insert(session_id.to_string());
    }

    async fn delivered_count(&self, session_id: &str) -> usize {
        self.delivered
            .read()
            .await
            .get(session_id)
            .map(|items| items.len())
            .unwrap_or(0)
    }

    async fn batch_delivered_count(&self, session_id: &str) -> usize {
        self.batch_delivered
            .read()
            .await
            .get(session_id)
            .map(|items| items.iter().map(|batch| batch.len()).sum())
            .unwrap_or(0)
    }
}

#[async_trait]
impl RouterSessionManager for TestSessionManager {
    async fn send_to_session(
        &self,
        session_id: &SessionId,
        message: &PushMessageRequest,
    ) -> Result<()> {
        if !self.is_session_online(session_id).await {
            anyhow::bail!("session offline");
        }
        self.delivered
            .write()
            .await
            .entry(session_id.clone())
            .or_default()
            .push(message.clone());
        Ok(())
    }

    async fn send_to_sessions(
        &self,
        sessions: &[SessionId],
        message: &PushMessageRequest,
    ) -> Result<Vec<Result<()>>> {
        let mut results = Vec::with_capacity(sessions.len());
        for session_id in sessions {
            results.push(self.send_to_session(session_id, message).await);
        }
        Ok(results)
    }

    async fn send_batch_to_session(
        &self,
        session_id: &SessionId,
        messages: &[PushMessageRequest],
    ) -> Result<()> {
        if !self.is_session_online(session_id).await {
            anyhow::bail!("session offline");
        }
        self.batch_delivered
            .write()
            .await
            .entry(session_id.clone())
            .or_default()
            .push(messages.to_vec());
        Ok(())
    }

    async fn is_session_online(&self, session_id: &SessionId) -> bool {
        self.online_sessions.read().await.contains(session_id)
    }

    async fn get_online_sessions(&self) -> Vec<SessionId> {
        self.online_sessions.read().await.iter().cloned().collect()
    }
}

async fn create_test_router() -> (Arc<MessageRouter>, Arc<TestSessionManager>) {
    let user_status_cache: Arc<dyn TwoLevelCache<u64, UserOnlineStatus>> =
        Arc::new(L1L2Cache::local_only(1024, Duration::from_secs(300)));
    let offline_queue: Arc<dyn TwoLevelCache<u64, Vec<OfflineMessage>>> =
        Arc::new(L1L2Cache::local_only(1024, Duration::from_secs(300)));
    let session_manager = Arc::new(TestSessionManager::default());

    let router = Arc::new(MessageRouter::new(
        MessageRouterConfig {
            offline_message_ttl: 24 * 3600,
            max_retry_count: 3,
            max_batch_size: 50,
            route_timeout_ms: 3000,
            max_offline_queue_size: 1000,
        },
        user_status_cache,
        offline_queue,
        session_manager.clone(),
    ));

    (router, session_manager)
}

fn create_test_message(from_uid: u64, channel_id: u64, content: &str) -> PushMessageRequest {
    let mut message = PushMessageRequest::new();
    message.from_uid = from_uid;
    message.channel_id = channel_id;
    message.channel_type = 2;
    message.message_type = 1;
    message.payload = content.as_bytes().to_vec();
    message
}

#[tokio::test]
async fn device_online_offline_updates_user_status() {
    let (router, session_manager) = create_test_router().await;
    let user_id: UserId = 10001;
    let device_id: DeviceId = "device-1".to_string();
    let session_id: SessionId = "session-1".to_string();

    assert!(router
        .get_user_online_status(&user_id)
        .await
        .expect("status query")
        .is_none());

    session_manager.set_online(&session_id).await;
    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .expect("register online");

    let status = router
        .get_user_online_status(&user_id)
        .await
        .expect("status query")
        .expect("user online");
    assert_eq!(status.user_id, user_id);
    assert_eq!(status.devices.len(), 1);
    assert_eq!(status.devices[0].device_id, device_id);
    assert_eq!(status.devices[0].session_id, session_id);

    router
        .register_device_offline(&user_id, &device_id, None)
        .await
        .expect("register offline");
    assert!(router
        .get_user_online_status(&user_id)
        .await
        .expect("status query")
        .is_none());
}

#[tokio::test]
async fn route_message_to_online_user_delivers_to_all_online_devices() {
    let (router, session_manager) = create_test_router().await;
    let user_id: UserId = 10002;

    for (device_id, session_id) in [
        ("device-mobile", "session-mobile"),
        ("device-desktop", "session-desktop"),
        ("device-tablet", "session-tablet"),
    ] {
        session_manager.set_online(session_id).await;
        router
            .register_device_online(&user_id, &device_id.to_string(), &session_id.to_string(), "mobile")
            .await
            .expect("register online");
    }

    let result = router
        .route_message_to_user(&user_id, create_test_message(20001, 30001, "hello"))
        .await
        .expect("route to online user");

    assert_eq!(result.success_count, 3);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 0);
    assert_eq!(session_manager.delivered_count("session-mobile").await, 1);
    assert_eq!(session_manager.delivered_count("session-desktop").await, 1);
    assert_eq!(session_manager.delivered_count("session-tablet").await, 1);
}

#[tokio::test]
async fn route_message_to_offline_user_enqueues_offline_message() {
    let (router, _session_manager) = create_test_router().await;
    let user_id: UserId = 10003;

    let result = router
        .route_message_to_user(&user_id, create_test_message(20002, 30002, "offline"))
        .await
        .expect("route to offline user");

    assert_eq!(result.success_count, 0);
    assert_eq!(result.failed_count, 0);
    assert_eq!(result.offline_count, 1);
}

#[tokio::test]
async fn deliver_offline_messages_batch_flushes_queue_when_device_comes_online() {
    let (router, session_manager) = create_test_router().await;
    let user_id: UserId = 10004;
    let device_id: DeviceId = "device-batch".to_string();
    let session_id: SessionId = "session-batch".to_string();

    for idx in 0..3 {
        router
            .route_message_to_user(
                &user_id,
                create_test_message(20003, 40000 + idx, &format!("offline-{idx}")),
            )
            .await
            .expect("enqueue offline message");
    }

    session_manager.set_online(&session_id).await;
    router
        .register_device_online(&user_id, &device_id, &session_id, "mobile")
        .await
        .expect("register online");

    let delivered = router
        .deliver_offline_messages_batch(&user_id, &device_id, 2)
        .await
        .expect("deliver offline batch");
    assert_eq!(delivered, 3);
    assert_eq!(session_manager.batch_delivered_count(&session_id).await, 3);
}
