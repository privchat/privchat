//! Isolated PostgreSQL acceptance for trusted private-assistant Directs and PTS cursors.
mod common;
use base64::Engine;
use privchat::repository::{PgChannelRepository, PgMessageRepository};
use privchat::service::assistant_direct;
use privchat::service::ChannelService;
use sqlx::PgPool;
use std::sync::Arc;

async fn fixture(name: &str) -> (common::ScratchDatabase, Arc<PgPool>) {
    let db = common::ScratchDatabase::create(name)
        .await
        .expect("PRIVCHAT_TEST_DATABASE_URL required; never skip");
    privchat::migrate::apply_pending(&mut db.connect().await, |_| {})
        .await
        .expect("real migrations");
    let pool = Arc::new(PgPool::connect(&db.url()).await.unwrap());
    for (uid, kind) in [(71001_i64, 0_i16), (71002, 3), (71003, 0), (71004, 2)] {
        sqlx::query("INSERT INTO privchat_users (user_id,user_type,status,username,qr_key) VALUES ($1,$2,0,$3,$4)")
            .bind(uid).bind(kind).bind(format!("assistant_test_{uid}")).bind(format!("qr_test_{uid}"))
            .execute(pool.as_ref()).await.unwrap();
    }
    (db, pool)
}

#[tokio::test]
async fn trusted_ensure_is_concurrent_idempotent_owner_only_and_hydrates_real_direct() {
    let (_db, pool) = fixture("assistant_ensure").await;
    let (a, b) = tokio::join!(
        assistant_direct::ensure(&pool, 71001, 71002),
        assistant_direct::ensure(&pool, 71001, 71002)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.0, b.0);
    assert_ne!(a.1, b.1);
    assert!(assistant_direct::ensure(&pool, 71003, 71002).await.is_err());
    assert!(assistant_direct::ensure(&pool, 71001, 71004).await.is_err());
    assert!(assistant_direct::ensure(&pool, 71002, 71001).await.is_err());
    assert_eq!(
        assistant_direct::owner_channel(&pool, 71001, 71002)
            .await
            .unwrap(),
        Some(a.0)
    );
    assert_eq!(
        assistant_direct::owner_channel(&pool, 71003, 71002)
            .await
            .unwrap(),
        None
    );
    let service =
        ChannelService::new_with_repository(Arc::new(PgChannelRepository::new(pool.clone())));
    let (id, created) = service
        .get_or_create_direct_channel(71001, 71002, None, None)
        .await
        .unwrap();
    assert_eq!(id, a.0);
    assert!(!created);
    let channel = service.get_channel_opt(id).await.unwrap();
    assert!(channel.is_member(71001));
    assert!(channel.is_member(71002));
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM privchat_user_channels WHERE channel_id=$1")
            .bind(id as i64)
            .fetch_one(pool.as_ref())
            .await
            .unwrap();
    assert_eq!(count, 2);
    let event = assistant_direct::received_event(&pool, id, 100, 71001, 71002, 1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.event_type, "assistant.message_received");
    let body: serde_json::Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(event.payload)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        body,
        serde_json::json!({"channel_id":id,"server_message_id":100,"from_user_id":71001,"assistant_uid":71002})
    );
    assert!(
        assistant_direct::received_event(&pool, id, 100, 71003, 71002, 1)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        assistant_direct::received_event(&pool, id, 100, 71002, 71001, 1)
            .await
            .unwrap()
            .is_none()
    );
    sqlx::query("UPDATE privchat_users SET status=2 WHERE user_id=71002")
        .execute(pool.as_ref())
        .await
        .unwrap();
    assert_eq!(
        assistant_direct::owner_channel(&pool, 71001, 71002)
            .await
            .unwrap(),
        None
    );
    assert!(assistant_direct::ensure(&pool, 71001, 71002).await.is_err());
    pool.close().await;
}

#[tokio::test]
async fn cursor_orders_by_committed_pts_not_smid_and_stops_at_projection_gap() {
    let (_db, pool) = fixture("assistant_cursor").await;
    let (channel, _) = assistant_direct::ensure(&pool, 71001, 71002).await.unwrap();
    let repo = PgMessageRepository::new(pool.clone());
    // Deliberately decreasing IDs with increasing PTS: the second ID was reserved earlier.
    for (id, pts) in [(900_u64, 1_i64), (800, 2), (700, 3)] {
        let text = if id == 800 {
            r#"{"text":"停止开发"}"#
        } else {
            "owner text"
        };
        let mut message = privchat::model::message::Message::new(
            channel,
            71001,
            text.into(),
            privchat_protocol::ContentMessageType::Text,
        );
        message.message_id = id;
        let event = privchat_protocol::CanonicalTimelineEvent::NewMessage(
            privchat_protocol::NewMessageEvent {
                message_type: privchat_protocol::ContentMessageType::Text,
                payload: privchat_protocol::MessagePayloadEnvelope {
                    content: text.into(),
                    metadata: None,
                    reply_to_message_id: None,
                    mentioned_user_ids: vec![],
                    message_source: None,
                },
            },
        );
        let committed = repo
            .create_message_and_commit_atomic(privchat::repository::AtomicMessageCommitRequest {
                message,
                event,
                dedup_key: None,
                client_registry_claim: None,
                attachment_refs: vec![],
                channel_type: 1,
                sender_username: None,
            })
            .await
            .unwrap();
        assert_eq!(committed.message.pts, Some(pts));
        assert!(committed.inserted);
    }
    let first = repo.list_messages_after(channel, 0, 1).await.unwrap();
    assert_eq!(first[0]["message_id"], 900);
    assert_eq!(first[0]["pts"], 1);
    let second = repo.list_messages_after(channel, 900, 1).await.unwrap();
    assert_eq!(second[0]["message_id"], 800);
    assert_eq!(second[0]["pts"], 2);
    assert_eq!(second[0]["content"], r#"{"text":"停止开发"}"#);
    let third = repo.list_messages_after(channel, 800, 50).await.unwrap();
    assert_eq!(third[0]["message_id"], 700);
    assert_eq!(third[0]["pts"], 3);
    assert!(repo
        .list_messages_after(channel, 700, 50)
        .await
        .unwrap()
        .is_empty());
    assert!(repo
        .list_messages_after(channel + 1, 900, 50)
        .await
        .is_err());
    assert!(repo.list_messages_after(channel, 999, 50).await.is_err());
    let (old, _) = repo
        .list_messages_admin(Some(channel), None, None, None, 1, 50)
        .await
        .unwrap();
    assert_eq!(old.len(), 3);
    assert!(old[0]["message_type"].is_string());
    sqlx::query("DELETE FROM privchat_messages WHERE channel_id=$1 AND message_id=800")
        .bind(channel as i64)
        .execute(pool.as_ref())
        .await
        .unwrap();
    assert!(
        repo.list_messages_after(channel, 900, 50)
            .await
            .unwrap()
            .is_empty(),
        "must not silently advance past a missing committed projection"
    );
    pool.close().await;
}

use async_trait::async_trait;
use msgtrans::{Packet, Responder, SessionHandler, SessionId, SessionSender};
use privchat::infra::connection_manager::ConnectionManager;
use privchat_protocol::protocol::PushMessageRequest;
use std::time::Duration;

// Only test authentication is abbreviated. All outbound messages use real TCP and
// the production MessageService -> durable outbox -> router -> connection path.
struct SocketAuth(Arc<ConnectionManager>);
#[async_trait]
impl SessionHandler for SocketAuth {
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {}
    async fn on_request(&self, sid: SessionId, packet: Packet, responder: Responder) {
        let scope = String::from_utf8(packet.payload().to_vec()).unwrap();
        self.0
            .register_connection_with_scopes(71002, scope.clone(), sid, vec![scope])
            .await
            .unwrap();
        responder.respond(b"bound".as_slice()).await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_service_commit_and_router_never_deliver_chat_to_same_uid_agent_node() {
    use msgtrans::{
        ClientEvent, TcpClientConfig, TcpServerConfig, TransportClientBuilder,
        TransportServerBuilder,
    };
    use privchat::infra::{
        message_router::{MessageRouter, MessageRouterConfig},
        redis::RedisClient,
    };
    use privchat::service::{
        CommittedTimelineDeliveryService, MessageService, OfflineQueueService,
        ServerSendMessageRequest, UnreadCountService,
    };
    let (_db, pool) = fixture("assistant_delivery").await;
    let channels = Arc::new(ChannelService::new_with_repository(Arc::new(
        PgChannelRepository::new(pool.clone()),
    )));
    let (channel, _) = channels
        .ensure_assistant_direct(71001, 71002)
        .await
        .unwrap();
    let manager = Arc::new(ConnectionManager::new());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let server = Arc::new(
        TransportServerBuilder::new()
            .protocol(TcpServerConfig::new(&addr).unwrap())
            .build(Arc::new(SocketAuth(manager.clone())))
            .await
            .unwrap(),
    );
    let serving = {
        let s = server.clone();
        tokio::spawn(async move {
            s.serve().await.unwrap();
        })
    };
    manager.set_transport_server(server.clone()).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut node = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(&addr).unwrap())
        .build()
        .await
        .unwrap();
    let mut full = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(&addr).unwrap())
        .build()
        .await
        .unwrap();
    let mut node_events = node.events().await.unwrap();
    let mut full_events = full.events().await.unwrap();
    node.connect().await.unwrap();
    full.connect().await.unwrap();
    node.request(b"agent_node".as_slice()).await.unwrap();
    full.request(b"im".as_slice()).await.unwrap();
    let (node_tx, mut node_rx) = tokio::sync::mpsc::unbounded_channel();
    let (full_tx, mut full_rx) = tokio::sync::mpsc::unbounded_channel();
    let node_reader = tokio::spawn(async move {
        while let Some(e) = node_events.next().await {
            match e {
                ClientEvent::Message(m) => {
                    node_tx.send(m.payload().to_vec()).unwrap();
                }
                ClientEvent::Request(r) => {
                    node_tx.send(r.payload().to_vec()).unwrap();
                    r.respond_detached(
                        privchat_protocol::encode_message(
                            &privchat_protocol::protocol::PushMessageResponse {
                                succeed: true,
                                message: None,
                            },
                        )
                        .unwrap(),
                    );
                }
                _ => {}
            }
        }
    });
    let full_reader = tokio::spawn(async move {
        while let Some(e) = full_events.next().await {
            match e {
                ClientEvent::Message(m) => {
                    full_tx.send(m.payload().to_vec()).unwrap();
                }
                ClientEvent::Request(r) => {
                    full_tx.send(r.payload().to_vec()).unwrap();
                    r.respond_detached(
                        privchat_protocol::encode_message(
                            &privchat_protocol::protocol::PushMessageResponse {
                                succeed: true,
                                message: None,
                            },
                        )
                        .unwrap(),
                    );
                }
                _ => {}
            }
        }
    });
    let router = Arc::new(MessageRouter::new(
        MessageRouterConfig::default(),
        manager.clone(),
    ));
    let redis = Arc::new(
        RedisClient::new(&privchat::config::RedisConfig {
            url: std::env::var("PRIVCHAT_TEST_REDIS_URL").expect("isolated Redis required"),
            pool_size: 4,
            min_idle: 1,
            connection_timeout_secs: 3,
            command_timeout_ms: 3000,
            idle_timeout_secs: 30,
        })
        .await
        .unwrap(),
    );
    let repo = Arc::new(PgMessageRepository::new(pool.clone()));
    let offline = Arc::new(OfflineQueueService::new(redis));
    let delivery = Arc::new(CommittedTimelineDeliveryService::new(
        pool.clone(),
        router.clone(),
        repo.clone(),
        offline.clone(),
        Arc::new(tokio::sync::Semaphore::new(4)),
        "assistant-test",
    ));
    let messages = MessageService::new(
        channels,
        repo.clone(),
        Arc::new(privchat::model::pts::UserMessageIndex::new()),
        offline,
        Arc::new(UnreadCountService::new(Arc::new(
            privchat::infra::cache::CacheManager::new(),
        ))),
        Arc::new(privchat::service::message_history_service::MessageHistoryService::new(100)),
        delivery,
    );
    let req = |sender, content: &str, key: Option<&str>| ServerSendMessageRequest {
        channel_id: channel,
        sender_id: sender,
        content: content.into(),
        message_type: privchat_protocol::ContentMessageType::Text,
        metadata: serde_json::json!({}),
        channel_type: 1,
        recipient_user_ids: vec![71001, 71002],
        dedup_key: key.map(str::to_string),
        attachment_refs_override: None,
    };
    let sent = messages
        .send_message(req(71001, "private owner text", Some("owner-request")))
        .await
        .unwrap();
    let received = tokio::time::timeout(Duration::from_secs(10), full_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let push: PushMessageRequest = privchat_protocol::decode_message(&received).unwrap();
    assert_eq!(push.server_message_id, sent.message_id);
    assert!(String::from_utf8_lossy(&push.payload).contains("private owner text"));
    assert!(
        tokio::time::timeout(Duration::from_millis(250), node_rx.recv())
            .await
            .is_err(),
        "agent_node leaked committed owner content"
    );
    // Exercise user broadcast AND exact-device/session exits with sync notifications.
    let node_sid = manager
        .get_user_connections(71002)
        .await
        .into_iter()
        .find(|c| c.device_id == "agent_node")
        .unwrap()
        .session_id;
    let mut hint = PushMessageRequest::new();
    hint.topic = privchat_protocol::ENTITY_INVALIDATION_PUSH_TOPIC_V1.into();
    let report = manager.send_push_to_user(71002, &hint).await.unwrap();
    assert_eq!(report.successful_count(), 1);
    assert_eq!(
        manager
            .send_push_to_device(71002, "agent_node", &hint)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        manager.send_push_to_session(node_sid, &hint).await.unwrap(),
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), node_rx.recv())
            .await
            .is_err(),
        "agent_node leaked sync notification"
    );
    let reply = messages
        .send_message(req(
            71002,
            "ordinary assistant reply",
            Some("assistant-reply"),
        ))
        .await
        .unwrap();
    let retried = messages
        .send_message(req(
            71002,
            "ordinary assistant reply",
            Some("assistant-reply"),
        ))
        .await
        .unwrap();
    assert_eq!(reply.message_id, retried.message_id);
    assert_eq!(reply.pts, retried.pts);
    assert!(!retried.inserted);
    assert!(messages
        .send_message(req(71002, "changed reply", Some("assistant-reply")))
        .await
        .is_err());
    assert!(messages
        .send_message(req(71002, "no key", None))
        .await
        .is_err());
    // Room is still delivered to the exact same restricted session.
    let mut room = PushMessageRequest::new();
    room.channel_type = 3;
    room.channel_id = 987;
    room.payload = b"room traffic".to_vec();
    assert_eq!(
        manager.send_push_to_session(node_sid, &room).await.unwrap(),
        1
    );
    let room_bytes = tokio::time::timeout(Duration::from_secs(2), node_rx.recv())
        .await
        .unwrap()
        .unwrap();
    let room_received: PushMessageRequest = privchat_protocol::decode_message(&room_bytes).unwrap();
    assert_eq!(room_received.payload, b"room traffic");
    node.disconnect().await.unwrap();
    full.disconnect().await.unwrap();
    server.shutdown_with_timeout(Duration::from_secs(2)).await;
    serving.abort();
    node_reader.abort();
    full_reader.abort();
    tokio::time::sleep(Duration::from_millis(100)).await;
    pool.close().await;
}

#[tokio::test]
async fn ordinary_owner_send_bypasses_nonfriend_privacy_but_not_blacklist_or_untrusted_direct() {
    use privchat::service::send_authorization::{
        authorize_send_to_channel, SendAuthorizationDeps, SendRefusal,
    };
    use privchat::service::{BlacklistService, FriendService, PrivacyService, QRCodeService};
    let (_db, pool) = fixture("assistant_authz").await;
    let channels = Arc::new(ChannelService::new_with_repository(Arc::new(
        PgChannelRepository::new(pool.clone()),
    )));
    let (id, _) = channels
        .ensure_assistant_direct(71001, 71002)
        .await
        .unwrap();
    let cache = Arc::new(
        privchat::infra::CacheManager::new(privchat::config::CacheConfig {
            l1_max_memory_mb: 1,
            ..Default::default()
        })
        .await
        .unwrap(),
    );
    let friends = Arc::new(FriendService::new(pool.clone()));
    let blacklist = Arc::new(BlacklistService::new(pool.clone(), cache.clone()));
    let privacy = Arc::new(PrivacyService::new(
        cache,
        channels.clone(),
        friends.clone(),
        Arc::new(QRCodeService::new()),
    ));
    let deps = SendAuthorizationDeps {
        channel_service: channels.clone(),
        friend_service: friends,
        blacklist_service: blacklist.clone(),
        privacy_service: privacy,
    };
    let channel = channels.get_channel_opt(id).await.unwrap();
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, 71001).await,
        Ok(())
    );
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, 71002).await,
        Ok(())
    );
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, 71003).await,
        Err(SendRefusal::NotAMember)
    );
    blacklist
        .add_to_blacklist(71002, 71001, None)
        .await
        .unwrap();
    assert_eq!(
        authorize_send_to_channel(&deps, &channel, 71001).await,
        Err(SendRefusal::BlockedByPeer)
    );
    let (untrusted, _) = channels
        .get_or_create_direct_channel(71003, 71002, None, None)
        .await
        .unwrap();
    let untrusted = channels.get_channel_opt(untrusted).await.unwrap();
    assert_eq!(
        authorize_send_to_channel(&deps, &untrusted, 71003).await,
        Err(SendRefusal::NotAMember)
    );
    pool.close().await;
}

#[tokio::test]
async fn canonical_direct_hydration_survives_cold_cache_and_service_restart_without_participants() {
    use privchat::repository::ChannelRepository;
    let (_db, pool) = fixture("direct_hydration").await;
    let repo = Arc::new(PgChannelRepository::new(pool.clone()));
    let normal = ChannelService::new_with_repository(repo.clone());
    let (id, _) = normal
        .get_or_create_direct_channel(71001, 71003, None, None)
        .await
        .unwrap();
    sqlx::query("DELETE FROM privchat_channel_participants WHERE channel_id=$1")
        .bind(id as i64)
        .execute(pool.as_ref())
        .await
        .unwrap();
    let loaded = repo.find_by_id(id).await.unwrap().unwrap();
    assert_eq!(loaded.members.len(), 2);
    for uid in [71001, 71003] {
        let member = loaded.members.get(&uid).unwrap();
        assert!(loaded.is_member(uid));
        assert!(member.permissions.can_send_message);
    }
    // A stale participant mirror must never add a third Direct member.
    sqlx::query(
        "INSERT INTO privchat_channel_participants (channel_id,user_id,role) VALUES ($1,71004,2)",
    )
    .bind(id as i64)
    .execute(pool.as_ref())
    .await
    .unwrap();
    for _ in 0..2 {
        let restarted = ChannelService::new_with_repository(repo.clone());
        let cold = restarted.get_channel_opt(id).await.unwrap();
        assert_eq!(cold.members.len(), 2);
        assert!(!cold.members.contains_key(&71004));
        let (again, created) = restarted
            .get_or_create_direct_channel(71001, 71003, None, None)
            .await
            .unwrap();
        assert_eq!(again, id);
        assert!(!created);
        let warm = restarted.get_channel_opt(id).await.unwrap();
        assert_eq!(warm.members.len(), 2);
        assert!(warm.members.contains_key(&71001));
        assert!(warm.members.contains_key(&71003));
    }
    pool.close().await;
}

struct QrSocket(Arc<ConnectionManager>);
#[async_trait]
impl SessionHandler for QrSocket {
    async fn on_message(&self, _: SessionId, _: Packet, _: SessionSender) {}
    async fn on_request(&self, sid: SessionId, packet: Packet, responder: Responder) {
        match packet.payload().as_ref() {
            b"connecting" => self.0.register_connecting(sid),
            b"agent_node" => self
                .0
                .register_connection_with_scopes(
                    71002,
                    "qr-node".into(),
                    sid,
                    vec!["agent_node".into()],
                )
                .await
                .unwrap(),
            _ => {}
        }
        responder
            .respond(sid.as_u64().to_string().into_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauth_qr_production_publisher_delivers_without_opening_an_agent_node_chat_bypass() {
    use msgtrans::{
        ClientEvent, TcpClientConfig, TcpServerConfig, TransportClientBuilder,
        TransportServerBuilder,
    };
    use privchat::service::qr_login_publisher::{PushOutcome, QrLoginPublisher};
    use privchat_protocol::rpc::qr_login::QrLoginPushEvent;
    let manager = Arc::new(ConnectionManager::new());
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr = format!("127.0.0.1:{port}");
    let server = Arc::new(
        TransportServerBuilder::new()
            .protocol(TcpServerConfig::new(&addr).unwrap())
            .build(Arc::new(QrSocket(manager.clone())))
            .await
            .unwrap(),
    );
    manager.set_transport_server(server.clone()).await;
    let serving = {
        let s = server.clone();
        tokio::spawn(async move {
            s.serve().await.unwrap();
        })
    };
    let mut client = TransportClientBuilder::new()
        .protocol(TcpClientConfig::new(&addr).unwrap())
        .build()
        .await
        .unwrap();
    let mut events = client.events().await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client.connect().await.is_ok() {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let publisher = QrLoginPublisher::new();
    let bytes = client.request(b"unindexed".as_slice()).await.unwrap();
    let sid = SessionId::new(std::str::from_utf8(&bytes).unwrap().parse().unwrap());
    for registration in ["unindexed", "connecting"] {
        let bytes = client.request(registration.as_bytes()).await.unwrap();
        assert_eq!(sid.as_u64().to_string().as_bytes(), bytes.as_ref());
        for state in ["scanned", "authorized", "rejected", "expired"] {
            let scene = format!("qr-{registration}");
            publisher.bind(scene.clone(), sid);
            let terminal = state != "scanned";
            let event = QrLoginPushEvent {
                event: format!("qr_login.{state}"),
                scene_id: scene.clone(),
                state: state.into(),
                data: Some(serde_json::json!({"test":"qr production exit"})),
            };
            assert_eq!(
                publisher
                    .push_event(&manager, event.clone(), terminal)
                    .await,
                PushOutcome::Delivered
            );
            let received = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Some(ClientEvent::Message(m)) = events.next().await {
                        break privchat_protocol::decode_message::<PushMessageRequest>(m.payload())
                            .unwrap();
                    }
                }
            })
            .await
            .unwrap();
            assert_eq!(received.topic, event.event);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&received.payload).unwrap(),
                serde_json::to_value(&event).unwrap()
            );
            assert_eq!(
                publisher.lookup_session(&scene),
                if terminal { None } else { Some(sid) }
            );
        }
    }
    let mut chat = PushMessageRequest::new();
    chat.channel_type = 1;
    chat.channel_id = 101;
    chat.payload = b"private owner text".to_vec();
    assert_eq!(
        manager
            .send_unauth_event_to_session(sid, &chat)
            .await
            .unwrap(),
        0
    );
    // Merely forging a QR topic cannot send an ordinary chat through this port.
    let mut forged = PushMessageRequest::new();
    forged.topic = "qr_login.authorized".into();
    forged.payload = chat.payload.clone();
    assert_eq!(
        manager
            .send_unauth_event_to_session(sid, &forged)
            .await
            .unwrap(),
        0
    );
    client.request(b"agent_node".as_slice()).await.unwrap();
    assert_eq!(
        manager
            .send_push_to_user(71002, &chat)
            .await
            .unwrap()
            .attempted,
        0
    );
    assert_eq!(
        manager
            .send_push_to_device(71002, "qr-node", &chat)
            .await
            .unwrap(),
        0
    );
    assert_eq!(manager.send_push_to_session(sid, &chat).await.unwrap(), 0);
    assert_eq!(
        manager
            .send_unauth_event_to_session(sid, &chat)
            .await
            .unwrap(),
        0
    );
    publisher.bind("node-scene".into(), sid);
    assert_eq!(
        publisher
            .push_event(
                &manager,
                QrLoginPushEvent {
                    event: "qr_login.authorized".into(),
                    scene_id: "node-scene".into(),
                    state: "authorized".into(),
                    data: None
                },
                true
            )
            .await,
        PushOutcome::NoSubscriber
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(150), async {
            loop {
                if matches!(events.next().await, Some(ClientEvent::Message(_))) {
                    break;
                }
            }
        })
        .await
        .is_err(),
        "agent_node must not receive chat or use the unauth QR bypass"
    );
    client.disconnect().await.unwrap();
    server.shutdown_with_timeout(Duration::from_secs(2)).await;
    serving.abort();
}
