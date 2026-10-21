//! Explicit loopback-only acceptance against an isolated, running IM fixture.
use msgtrans::{
    ClientEvent, RequestOptions, TransportClient, TransportClientBuilder, WebSocketClientConfig,
};
use privchat_protocol::{
    decode_message, encode_message,
    protocol::{
        AuthorizationRequest, AuthorizationResponse, DeviceType, MessageType, PublishRequest,
        RpcRequest, RpcResponse,
    },
};
use privchat_protocol::{CanonicalTimelineEvent, FlatBufferMessage};
use serde_json::{json, Value};
use std::time::Duration;

async fn rpc(client: &TransportClient, route: &str, body: Value) -> RpcResponse {
    let req = RpcRequest {
        route: route.into(),
        body: serde_json::to_vec(&body).unwrap(),
    };
    let bytes = client
        .request_with_options(
            encode_message(&req).unwrap().into(),
            RequestOptions::new()
                .biz_type(MessageType::RpcRequest as u8)
                .timeout(Duration::from_secs(10)),
        )
        .await
        .expect("RPC transport");
    decode_message(&bytes).expect("RPC envelope")
}
fn data(response: RpcResponse) -> Value {
    assert_eq!(response.code, 0, "RPC refused: {}", response.message);
    serde_json::from_slice(&response.data.expect("RPC data")).unwrap()
}
type Events = tokio::sync::mpsc::UnboundedReceiver<(u8, Vec<u8>)>;
async fn login(account: &Value, app: &str) -> (TransportClient, Events) {
    let cfg = WebSocketClientConfig::new("ws://127.0.0.1:56880/gate").unwrap();
    let mut client = TransportClientBuilder::new()
        .protocol(cfg)
        .build()
        .await
        .unwrap();
    let mut incoming = client.events().await.unwrap();
    let (sender, events) = tokio::sync::mpsc::unbounded_channel();
    // Real clients ACK message pushes continuously, including login history.
    tokio::spawn(async move {
        while let Some(event) = incoming.next().await {
            let packet = match event {
                ClientEvent::Message(m) => (m.biz_type(), m.into_payload().to_vec()),
                ClientEvent::Request(r) => {
                    let packet = (r.biz_type(), r.payload().to_vec());
                    if packet.0 == MessageType::PushMessageRequest as u8 {
                        let ack = privchat_protocol::protocol::PushMessageResponse {
                            succeed: true,
                            message: None,
                        };
                        r.respond_with_options(
                            encode_message(&ack).unwrap(),
                            msgtrans::SendOptions::new()
                                .biz_type(MessageType::PushMessageResponse as u8),
                        )
                        .await
                        .expect("push ACK");
                    }
                    packet
                }
                _ => continue,
            };
            if sender.send(packet).is_err() {
                break;
            }
        }
    });
    client.connect().await.unwrap();
    let device = uuid::Uuid::new_v4().to_string();
    let result=data(rpc(&client,"account/auth/login",json!({"username":account["username"],"password":account["password"],"device_id":device,"device_info":{"device_id":device,"device_type":app,"app_id":app,"device_name":"Call acceptance test"}})).await);
    let mut request = AuthorizationRequest::new();
    request.auth_token = result["token"].as_str().expect("login token").into();
    request.device_info.device_id = device;
    request.device_info.app_id = app.into();
    request.device_info.device_type = if app == "ios" {
        DeviceType::iOS
    } else {
        DeviceType::MacOS
    };
    request.client_info.client_type = "call-acceptance-test".into();
    request.client_info.version = "1.0".into();
    let response = client
        .request_with_options(
            encode_message(&request).unwrap().into(),
            RequestOptions::new()
                .biz_type(MessageType::AuthorizationRequest as u8)
                .timeout(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let response: AuthorizationResponse = decode_message(&response).unwrap();
    assert!(
        response.success,
        "authorization failed: {:?}",
        response.error_message
    );
    (client, events)
}
async fn state(events: &mut Events, id: u64, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (kind, bytes) = events.recv().await.expect("event stream open");
            if kind != MessageType::PublishRequest as u8 {
                continue;
            }
            let event: PublishRequest = decode_message(&bytes).unwrap();
            if event.topic.as_deref() != Some("call/state") {
                continue;
            }
            let payload: Value = serde_json::from_slice(&event.payload).unwrap();
            if payload["call"]["call_id"] == id && payload["call"]["state"] == expected {
                break;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("direct call state {expected} for {id} without chat subscription"));
}

#[tokio::test]
#[ignore = "requires explicitly created isolated loopback IM fixture"]
async fn real_im_invite_multidevice_publish_end_and_exactly_once_history() {
    let root = std::env::var("PRIVCHAT_CALL_IM_RUNTIME").expect("isolated fixture required");
    assert!(root.starts_with("/tmp/privchat-call-im."));
    let db = std::fs::read_to_string(format!("{root}/db-name")).unwrap();
    assert!(
        db.starts_with("privchat_call_im_")
            && db.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    );
    let pool = sqlx::PgPool::connect(&format!("postgres://zoujiaqing@localhost/{db}"))
        .await
        .unwrap();
    let accounts: Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{root}/test-accounts.json")).unwrap(),
    )
    .unwrap();
    let (alice, mut alice_events) = login(&accounts["alice"], "ios").await;
    let (bob, mut bob_events) = login(&accounts["bob"], "ios").await;
    let (desktop, mut desktop_events) = login(&accounts["bob"], "macos").await;
    let capabilities = data(rpc(&alice, "call/capabilities", json!({})).await);
    assert_eq!(capabilities["enabled"], true);
    assert_eq!(capabilities["heartbeat_lease_seconds"], 60);
    let invite = json!({"command_id":uuid::Uuid::new_v4().to_string(),"channel_id":600001,"media_kind":"audio"});
    let offered = data(rpc(&alice, "call/invite", invite.clone()).await);
    let id = offered["call"]["call_id"].as_u64().unwrap();
    assert_eq!(offered, data(rpc(&alice, "call/invite", invite).await));
    state(&mut bob_events, id, "ringing").await;
    state(&mut desktop_events, id, "ringing").await;
    let active = data(rpc(&desktop, "call/active", json!({})).await);
    assert!(active["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["call_id"] == id));
    let join = json!({"command_id":uuid::Uuid::new_v4().to_string(),"call_id":id});
    let (phone_result, desktop_result) = tokio::join!(
        rpc(&bob, "call/join", join.clone()),
        rpc(&desktop, "call/join", join)
    );
    assert_ne!(phone_result.code == 0, desktop_result.code == 0);
    let (winner, loser, joined) = if phone_result.code == 0 {
        (&bob, &desktop, data(phone_result))
    } else {
        (&desktop, &bob, data(desktop_result))
    };
    assert!(joined["token"].as_str().is_some_and(|t| !t.is_empty()));
    let mut media_url = url::Url::parse("http://127.0.0.1:56780/rtc").unwrap();
    media_url
        .query_pairs_mut()
        .append_pair("access_token", joined["token"].as_str().unwrap())
        .append_pair("auto_subscribe", "1")
        .append_pair("protocol", "15")
        .append_pair("sdk", "rust");
    let media_response = reqwest::Client::new()
        .get(media_url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .unwrap_or_else(|_| panic!("Media handshake failed (URL redacted)"));
    assert_eq!(
        media_response.status(),
        101,
        "IM-issued credentials must authenticate with real SFU"
    );
    let media_connection = media_response.upgrade().await.unwrap();
    let rejected = rpc(
        loser,
        "call/heartbeat",
        json!({"command_id":uuid::Uuid::new_v4().to_string(),"call_id":id}),
    )
    .await;
    assert_eq!(
        rejected.code,
        privchat_protocol::ErrorCode::PermissionDenied.code() as i32
    );
    state(&mut alice_events, id, "active").await;
    let end = json!({"command_id":uuid::Uuid::new_v4().to_string(),"call_id":id});
    let ended = data(rpc(winner, "call/end", end.clone()).await);
    assert_eq!(ended["call"]["state"], "ended");
    assert_eq!(ended, data(rpc(winner, "call/end", end).await));
    state(&mut alice_events, id, "ended").await;
    let active = data(rpc(&bob, "call/active", json!({})).await);
    assert!(!active["calls"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["call_id"] == id));
    tokio::time::timeout(Duration::from_secs(20),async {
        loop {
            let complete:bool=sqlx::query_scalar("SELECT history_message_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM privchat_call_effects e WHERE e.call_id=c.call_id AND kind IN ('history','delete_room') AND NOT completed) FROM privchat_calls c WHERE call_id=$1").bind(id as i64).fetch_one(&pool).await.unwrap();
            if complete {break;} tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }).await.expect("normal timeline history and SFU cleanup");
    drop(media_connection);
    let delivered = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let (kind, bytes) = bob_events.recv().await.expect("message stream");
            if kind != MessageType::PushMessageRequest as u8 {
                continue;
            }
            let push: privchat_protocol::protocol::PushMessageRequest =
                decode_message(&bytes).unwrap();
            // NewMessage uses the existing rolling-compatible JSON envelope;
            // the canonical FlatBuffer is stored in the commit log.
            if let Ok(envelope) = serde_json::from_slice::<Value>(&push.payload) {
                if serde_json::from_str::<Value>(envelope["content"].as_str().unwrap_or(""))
                    .is_ok_and(|v| v["call_id"] == id && v["event"] == "call.ended")
                {
                    assert_eq!(
                        push.message_type,
                        privchat_protocol::ContentMessageType::System.as_u32()
                    );
                    assert!(push.message_seq > 0);
                    break push;
                }
            }
        }
    })
    .await
    .expect("real canonical summary push received without chat subscription");
    let canonical_bytes: Vec<u8> = sqlx::query_scalar(
        "SELECT canonical_event FROM privchat_commit_log WHERE server_msg_id=$1",
    )
    .bind(delivered.server_message_id as i64)
    .fetch_one(&pool)
    .await
    .unwrap();
    let CanonicalTimelineEvent::NewMessage(message) =
        CanonicalTimelineEvent::decode_fb(&canonical_bytes).unwrap()
    else {
        panic!("summary canonical event kind");
    };
    assert_eq!(
        serde_json::from_str::<Value>(&message.payload.content).unwrap()["call_id"],
        id
    );
    // Inject the crash window after MessageService committed but before effect completion.
    sqlx::query("UPDATE privchat_calls SET history_message_id=NULL WHERE call_id=$1")
        .bind(id as i64)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE privchat_call_effects SET completed=false,next_attempt_at=0 WHERE call_id=$1 AND kind='history'").bind(id as i64).execute(&pool).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let restored: bool = sqlx::query_scalar("SELECT history_message_id IS NOT NULL AND NOT EXISTS(SELECT 1 FROM privchat_call_effects e WHERE e.call_id=c.call_id AND kind='history' AND NOT completed) FROM privchat_calls c WHERE call_id=$1")
                .bind(id as i64).fetch_one(&pool).await.unwrap();
            if restored { break; }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }).await.expect("committed summary replay restores history marker and completes effect");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM privchat_messages WHERE channel_id=600001 AND metadata->>'call_id'=$1 AND pts IS NOT NULL").bind(id.to_string()).fetch_one(&pool).await.unwrap();
    assert_eq!(
        count, 1,
        "history replay must not duplicate normal timeline message"
    );
    let canonical:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_message_dispatch_outbox o JOIN privchat_calls c ON c.call_id=$1 JOIN privchat_messages m ON m.message_id=c.history_message_id AND m.channel_id=o.channel_id AND m.pts=o.pts)").bind(id as i64).fetch_one(&pool).await.unwrap();
    assert!(
        canonical,
        "summary must use normal timeline delivery outbox"
    );
    // Read through the authenticated history RPC, with no local message cache.
    let history = data(
        rpc(
            &bob,
            "message/history/get",
            json!({"channel_id":600001,"limit":100}),
        )
        .await,
    );
    let messages = history["messages"].as_array().expect("history messages");
    let summaries: Vec<_> = messages
        .iter()
        .filter(|message| {
            serde_json::from_str::<Value>(message["content"].as_str().unwrap_or(""))
                .is_ok_and(|content| content["event"] == "call.ended" && content["call_id"] == id)
        })
        .collect();
    assert_eq!(
        summaries.len(),
        1,
        "uncached RPC history returns summary exactly once"
    );
    assert_eq!(summaries[0]["message_type"], "system");
    assert!(summaries[0]["message_seq"].as_u64().unwrap() > 0);
    assert_eq!(summaries[0]["message_id"], delivered.server_message_id);
    assert_eq!(summaries[0]["message_seq"], delivered.message_seq);
    let repository = privchat::repository::message_repo::PgMessageRepository::new(
        std::sync::Arc::new(pool.clone()),
    );
    assert!(repository
        .find_message_by_dedup_key(&format!("call-summary:{id}"))
        .await
        .expect("dedup row decoding without optional sender projection")
        .is_some());
    // Inject an actual PostgreSQL visibility-read timeout, not an authorization
    // denial. The worker's outer snapshot SELECT is nonlocking; call/get's
    // FOR UPDATE must time out while this transaction holds the call row.
    let mut lock = pool.begin().await.unwrap();
    sqlx::query("SELECT call_id FROM privchat_calls WHERE call_id=$1 FOR UPDATE")
        .bind(id as i64)
        .execute(&mut *lock)
        .await
        .unwrap();
    let effect: i64 = sqlx::query_scalar("SELECT id FROM privchat_call_effects WHERE call_id=$1 AND kind='state' ORDER BY version DESC LIMIT 1")
        .bind(id as i64).fetch_one(&pool).await.unwrap();
    sqlx::query("UPDATE privchat_call_effects SET completed=false,next_attempt_at=0 WHERE id=$1")
        .bind(effect)
        .execute(&pool)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let row: (bool, i64) = sqlx::query_as(
                "SELECT completed,next_attempt_at FROM privchat_call_effects WHERE id=$1",
            )
            .bind(effect)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert!(
                !row.0,
                "P1: temporary DB read failure must not complete state effect"
            );
            if row.1 > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("visibility query timeout persisted durable retry");
    lock.rollback().await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let complete: bool =
                sqlx::query_scalar("SELECT completed FROM privchat_call_effects WHERE id=$1")
                    .bind(effect)
                    .fetch_one(&pool)
                    .await
                    .unwrap();
            if complete {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("state effect recovered after DB lock release");
    alice.disconnect().await.unwrap();
    bob.disconnect().await.unwrap();
    desktop.disconnect().await.unwrap();
    println!("PASS: normal login/auth, unsubscribed user Publish, active reconciliation, device race, terminal event, durable normal history replay, media cleanup, P1 DB-timeout durable retry and recovery");
}
