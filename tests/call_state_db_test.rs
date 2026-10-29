mod common;
use privchat::service::{call_config::CallConfig, call_service::CallService};
use privchat_protocol::rpc::call::*;
use serde_json::{json, Value};
use sqlx::PgPool;

async fn setup(name: &str) -> (common::ScratchDatabase, CallService) {
    let db = common::ScratchDatabase::create(name)
        .await
        .expect("isolated DB required");
    let mut conn = db.connect().await;
    privchat::migrate::apply_pending(&mut conn, |_| {})
        .await
        .unwrap();
    drop(conn);
    let pool = PgPool::connect(&db.url()).await.unwrap();
    for id in 1..=20i64 {
        sqlx::query("INSERT INTO privchat_users(user_id,username,qr_key) VALUES($1,$2,$2)")
            .bind(id)
            .bind(format!("calltest{id}"))
            .execute(&pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO privchat_channels(channel_id,channel_type,direct_user1_id,direct_user2_id) VALUES(100,0,1,2),(101,0,3,2)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO privchat_channel_participants(channel_id,user_id) VALUES(100,1),(100,2),(101,3),(101,2)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO privchat_groups(group_id,name,owner_id,qr_key) VALUES(200,'call test',1,'callgroup')").execute(&pool).await.unwrap();
    sqlx::query(
        "INSERT INTO privchat_channels(channel_id,channel_type,group_id) VALUES(200,1,200)",
    )
    .execute(&pool)
    .await
    .unwrap();
    for id in 1..=17i64 {
        sqlx::query("INSERT INTO privchat_group_members(group_id,user_id,role) VALUES(200,$1,$2)")
            .bind(id)
            .bind(if id == 1 { 0i16 } else { 2i16 })
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO privchat_channel_participants(channel_id,user_id,role) VALUES(200,$1,$2)",
        )
        .bind(id)
        .bind(if id == 1 { 0i16 } else { 2i16 })
        .execute(&pool)
        .await
        .unwrap();
    }
    let config = CallConfig {
        enabled: true,
        livekit_url: "ws://127.0.0.1:56780".into(),
        peer: privchat::service::call_config::CallPeerSettings {
            enabled: true,
            allow_turn: true,
            turn_urls: vec!["turn:127.0.0.1:56888?transport=udp".into()],
            ..Default::default()
        },
        ..Default::default()
    };
    let service = CallService::with_peer_secret(
        pool,
        config,
        Some((
            "test-key".into(),
            "test-secret-at-least-thirty-two-bytes".into(),
        )),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    (db, service)
}
fn invite(id: &str, channel: u64) -> CallInviteRequest {
    CallInviteRequest {
        supports_peer: true,
        command_id: id.into(),
        channel_id: channel,
        media_kind: CallMediaKind::Audio,
        invitee_ids: vec![],
    }
}
fn command(id: &str, call: u64) -> CallMutationRequest {
    CallMutationRequest {
        supports_peer: true,
        command_id: id.into(),
        call_id: call,
    }
}
fn call_id(value: &Value) -> u64 {
    value["call"]["call_id"].as_u64().unwrap()
}

async fn peer_setup(name: &str) -> (common::ScratchDatabase, CallService) {
    let (db, old) = setup(name).await;
    let mut config = old.config.clone();
    config.peer.enabled = true;
    config.peer.stun_urls = vec!["stun:127.0.0.1:53479".into()];
    config.peer.turn_urls = vec!["turn:127.0.0.1:53479?transport=udp".into()];
    let service = CallService::with_peer_secret(
        old.pool.clone(),
        config,
        Some(("test-key".into(), "test-secret".into())),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    (db, service)
}
fn peer_command(id: &str, call: u64) -> CallMutationRequest {
    CallMutationRequest {
        supports_peer: true,
        ..command(id, call)
    }
}
fn signal(id: &str, call: u64, revision: u32, kind: CallPeerSignalKind) -> CallPeerSignalRequest {
    CallPeerSignalRequest {
        command_id: id.into(),
        call_id: call,
        revision,
        kind,
        payload: if kind == CallPeerSignalKind::Ice {
            json!({"candidate":"candidate:test","sdpMid":"0","sdpMLineIndex":0}).to_string()
        } else {
            "v=0\r\ns=test fixture\r\n".into()
        },
    }
}

#[tokio::test]
async fn peer_transport_credentials_and_legacy_compatibility() {
    use base64::Engine;
    use hmac::{Hmac, Mac};
    let (_db, s) = peer_setup("peer_transport").await;
    assert!(s
        .invite(
            1,
            "a",
            CallInviteRequest {
                supports_peer: false,
                ..invite("old", 100)
            }
        )
        .await
        .is_err());
    let offered = s
        .invite(
            1,
            "a",
            CallInviteRequest {
                supports_peer: true,
                ..invite("offer", 100)
            },
        )
        .await
        .unwrap();
    let id = call_id(&offered);
    assert_eq!(offered["call"]["transport"], "webrtc");
    assert!(s
        .mutate(
            "join",
            2,
            "b",
            CallMutationRequest {
                supports_peer: false,
                ..command("oldjoin", id)
            }
        )
        .await
        .is_err());
    let a = s
        .mutate("join", 1, "a", peer_command("join-a", id))
        .await
        .unwrap();
    let b = s
        .mutate("join", 2, "b", peer_command("join-b", id))
        .await
        .unwrap();
    assert_eq!(a["url"], "");
    assert_eq!(a["token"], "");
    assert_eq!(a["peer"]["offerer"], true);
    assert_eq!(b["peer"]["self_user_id"], 2);
    assert_eq!(b["peer"]["peer_user_id"], 1);
    assert_eq!(b["peer"]["offerer"], false);
    let ice = &a["peer"]["ice_servers"][1];
    let mut mac =
        Hmac::<sha1::Sha1>::new_from_slice(b"isolated-turn-secret-not-production").unwrap();
    mac.update(ice["username"].as_str().unwrap().as_bytes());
    assert_eq!(
        ice["credential"],
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    );
    assert!(
        a["peer"]["credential_expires_at_ms"].as_i64().unwrap()
            > chrono::Utc::now().timestamp_millis()
    );
    let snapshot = s.get(1, id).await.unwrap();
    assert!(s.join_token(&snapshot, 1, "a", i64::MAX).is_err());
    s.media_cleanup(&snapshot, None).await.unwrap();
    // Existing call transport is immutable even if current peer selection is disabled.
    let mut off = s.clone();
    off.config.peer.enabled = false;
    assert_eq!(
        off.get(1, id).await.unwrap().transport,
        CallTransport::Webrtc
    );
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let renewed = s
        .mutate("join", 1, "a", peer_command("renew", id))
        .await
        .unwrap();
    assert!(
        renewed["peer"]["credential_expires_at_ms"]
            .as_i64()
            .unwrap()
            > a["peer"]["credential_expires_at_ms"].as_i64().unwrap()
    );
    assert_eq!(
        s.mutate("join", 1, "a", peer_command("join-a", id))
            .await
            .unwrap(),
        a
    );
    s.mutate("end", 1, "a", peer_command("end", id))
        .await
        .unwrap();
    let cleanup:i64=sqlx::query_scalar("SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind IN ('delete_room','remove_participant')").bind(id as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(cleanup, 0);
    let group = s
        .invite(
            1,
            "a",
            CallInviteRequest {
                invitee_ids: vec![2],
                ..invite("group", 200)
            },
        )
        .await
        .unwrap();
    assert_eq!(group["call"]["transport"], "livekit");
    let group_join = s
        .mutate("join", 2, "b", command("groupjoin", call_id(&group)))
        .await
        .unwrap();
    assert!(group_join["peer"].is_null());
    assert!(!group_join["token"].as_str().unwrap().is_empty());
}

#[tokio::test]
async fn peer_journal_auth_replay_revision_restart_and_terminal_cleanup() {
    let (_db, s) = peer_setup("peer_journal").await;
    let id = call_id(
        &s.invite(
            1,
            "a",
            CallInviteRequest {
                supports_peer: true,
                ..invite("invite", 100)
            },
        )
        .await
        .unwrap(),
    );
    let read = CallPeerEventsRequest {
        call_id: id,
        after_seq: 0,
    };
    assert!(s.peer_events(2, "b", read.clone()).await.is_err());
    s.mutate("join", 2, "b", peer_command("join", id))
        .await
        .unwrap();
    assert!(s
        .peer_signal(
            2,
            "b",
            signal("bad-offer", id, 1, CallPeerSignalKind::Offer)
        )
        .await
        .is_err());
    assert!(s
        .peer_signal(
            1,
            "other",
            signal("bad-device", id, 1, CallPeerSignalKind::Offer)
        )
        .await
        .is_err());
    let offer = signal("offer1", id, 1, CallPeerSignalKind::Offer);
    let first = s.peer_signal(1, "a", offer.clone()).await.unwrap();
    assert_eq!(first.next_seq, 1);
    s.peer_signal(1, "a", signal("ice", id, 1, CallPeerSignalKind::Ice))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(s.peer_signal(1, "a", offer.clone()).await.unwrap()).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    let mut conflict = offer.clone();
    conflict.payload.push('x');
    assert!(s.peer_signal(1, "a", conflict).await.is_err());
    assert!(s
        .mutate("heartbeat", 1, "a", peer_command("offer1", id))
        .await
        .is_err());
    assert!(s
        .peer_signal(2, "b", signal("join", id, 1, CallPeerSignalKind::Answer))
        .await
        .is_err());
    let events = s.peer_events(2, "b", read.clone()).await.unwrap();
    assert_eq!(events.events.len(), 2);
    assert_eq!(events.next_seq, 2);
    assert_eq!(events.events[0].from_user_id, 1);
    assert_eq!(events.events[0].payload, offer.payload);
    assert!(s
        .peer_events(1, "a", read.clone())
        .await
        .unwrap()
        .events
        .is_empty());
    assert!(s.peer_events(2, "wrong", read.clone()).await.is_err());
    assert!(s.peer_events(3, "c", read.clone()).await.is_err());
    assert!(s
        .peer_signal(
            1,
            "a",
            signal("badanswer", id, 1, CallPeerSignalKind::Answer)
        )
        .await
        .is_err());
    s.peer_signal(2, "b", signal("answer", id, 1, CallPeerSignalKind::Answer))
        .await
        .unwrap();
    assert!(s
        .peer_signal(2, "b", signal("answer2", id, 1, CallPeerSignalKind::Answer))
        .await
        .is_err());
    let restarted = CallService::with_peer_secret(
        s.pool.clone(),
        s.config.clone(),
        Some(("k".into(), "s".into())),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    assert_eq!(
        restarted
            .peer_events(1, "a", read.clone())
            .await
            .unwrap()
            .events
            .len(),
        1
    );
    s.peer_signal(1, "a", signal("restart", id, 2, CallPeerSignalKind::Offer))
        .await
        .unwrap();
    assert!(s
        .peer_signal(2, "b", signal("oldice", id, 1, CallPeerSignalKind::Ice))
        .await
        .is_err());
    assert!(s.peer_signal(1, "a", offer).await.is_err());
    sqlx::query("INSERT INTO privchat_blacklist(user_id,blocked_user_id) VALUES(1,2)")
        .execute(&s.pool)
        .await
        .unwrap();
    assert!(s.peer_events(2, "b", read.clone()).await.is_err());
    sqlx::query("DELETE FROM privchat_blacklist WHERE user_id=1 AND blocked_user_id=2")
        .execute(&s.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE privchat_channels SET direct_user2_id=3 WHERE channel_id=100")
        .execute(&s.pool)
        .await
        .unwrap();
    assert!(s.peer_events(2, "b", read.clone()).await.is_err());
    sqlx::query("UPDATE privchat_channels SET direct_user2_id=2 WHERE channel_id=100")
        .execute(&s.pool)
        .await
        .unwrap();
    s.mutate("end", 1, "a", peer_command("end", id))
        .await
        .unwrap();
    assert!(s.peer_events(1, "a", read).await.is_err());
    assert!(s
        .peer_signal(1, "a", signal("terminal", id, 3, CallPeerSignalKind::Offer))
        .await
        .is_err());
    for table in [
        "privchat_call_peer_events",
        "privchat_call_peer_commands",
        "privchat_call_peer_rates",
    ] {
        let count: i64 =
            sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE call_id=$1"))
                .bind(id as i64)
                .fetch_one(&s.pool)
                .await
                .unwrap();
        assert_eq!(count, 0);
    }
}

#[tokio::test]
async fn peer_journal_payload_rate_queue_and_expiry_bounds() {
    let (_db, s) = peer_setup("peer_bounds").await;
    let id = call_id(
        &s.invite(
            1,
            "a",
            CallInviteRequest {
                supports_peer: true,
                ..invite("invite", 100)
            },
        )
        .await
        .unwrap(),
    );
    s.mutate("join", 2, "b", peer_command("join", id))
        .await
        .unwrap();
    let mut huge = signal("huge", id, 1, CallPeerSignalKind::Offer);
    huge.payload = "x".repeat(65537);
    assert!(s.peer_signal(1, "a", huge).await.is_err());
    s.peer_signal(1, "a", signal("offer", id, 1, CallPeerSignalKind::Offer))
        .await
        .unwrap();
    let mut bad = signal("badice", id, 1, CallPeerSignalKind::Ice);
    bad.payload = "{}".into();
    assert!(s.peer_signal(1, "a", bad).await.is_err());
    sqlx::query("INSERT INTO privchat_call_peer_rates(call_id,user_id,operation,bucket,count) VALUES($1,2,'events',$2,40)").bind(id as i64).bind(chrono::Utc::now().timestamp_millis()/10000).execute(&s.pool).await.unwrap();
    assert!(s
        .peer_events(
            2,
            "b",
            CallPeerEventsRequest {
                call_id: id,
                after_seq: 0
            }
        )
        .await
        .is_err());
    sqlx::query("UPDATE privchat_call_peer_rates SET count=60,bucket=$2 WHERE call_id=$1 AND operation='signal'").bind(id as i64).bind(chrono::Utc::now().timestamp_millis()/10000).execute(&s.pool).await.unwrap();
    assert!(s
        .peer_signal(1, "a", signal("limited", id, 1, CallPeerSignalKind::Ice))
        .await
        .is_err());
    sqlx::query("DELETE FROM privchat_call_peer_rates WHERE call_id=$1")
        .bind(id as i64)
        .execute(&s.pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO privchat_call_peer_events SELECT $1,n,1,'ice','{}',1,$2 FROM generate_series(2,512) n").bind(id as i64).bind(chrono::Utc::now().timestamp_millis()+300000).execute(&s.pool).await.unwrap();
    assert!(s
        .peer_signal(1, "a", signal("full", id, 1, CallPeerSignalKind::Ice))
        .await
        .is_err());
    sqlx::query("UPDATE privchat_call_peer_events SET expires_at=0 WHERE call_id=$1")
        .bind(id as i64)
        .execute(&s.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE privchat_call_peer_commands SET expires_at=0 WHERE call_id=$1")
        .bind(id as i64)
        .execute(&s.pool)
        .await
        .unwrap();
    s.sweep().await.unwrap();
    assert!(s
        .peer_events(
            2,
            "b",
            CallPeerEventsRequest {
                call_id: id,
                after_seq: 0
            }
        )
        .await
        .is_err());
    s.peer_signal(1, "a", signal("restart", id, 2, CallPeerSignalKind::Offer))
        .await
        .unwrap();
    let events = s
        .peer_events(
            2,
            "b",
            CallPeerEventsRequest {
                call_id: id,
                after_seq: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].revision, 2);
}

#[tokio::test]
async fn peer_unavailable_rejects_new_dm_but_legacy_transport_still_joins() {
    let (_db, s) = setup("peer_legacy").await;
    let id = call_id(&s.invite(1, "a", invite("invite", 100)).await.unwrap());
    // A stored pre-peer snapshot lacks transport and must retain the SFU path.
    sqlx::query(
        "UPDATE privchat_calls SET document=document #- '{call,transport}' WHERE call_id=$1",
    )
    .bind(id as i64)
    .execute(&s.pool)
    .await
    .unwrap();
    let mut legacy = s.clone();
    legacy.config.peer.enabled = false;
    assert!(!legacy.capabilities().peer_enabled);
    assert!(legacy.capabilities().livekit_enabled);
    let join = legacy
        .mutate(
            "join",
            2,
            "b",
            CallMutationRequest {
                supports_peer: false,
                ..command("legacy-join", id)
            },
        )
        .await
        .unwrap();
    assert_eq!(join["call"]["transport"], "livekit");
    assert!(!join["token"].as_str().unwrap().is_empty());
    legacy
        .mutate("end", 2, "b", command("end", id))
        .await
        .unwrap();
    assert!(legacy.invite(1, "a", invite("new-dm", 100)).await.is_err());
    let mut peer_only = s.clone();
    peer_only.config.livekit_url.clear();
    assert!(peer_only.capabilities().peer_enabled);
    assert!(!peer_only.capabilities().livekit_enabled);
    assert!(peer_only
        .invite(
            1,
            "a",
            CallInviteRequest {
                invitee_ids: vec![2],
                ..invite("no-group", 200)
            }
        )
        .await
        .is_err());
}

#[tokio::test]
async fn peer_allow_turn_disabled_generates_no_credentials_and_sanitizes_replay() {
    let (_db, s) = peer_setup("peer_turn_opt_in").await;
    let id = call_id(&s.invite(1, "a", invite("invite", 100)).await.unwrap());
    let original = s.mutate("join", 1, "a", command("join", id)).await.unwrap();
    assert!(original["peer"]["ice_servers"][1]["credential"].is_string());
    let mut config = s.config.clone();
    config.livekit_url.clear();
    config.peer.allow_turn = false;
    let direct = CallService::with_peer_secret(s.pool.clone(), config.clone(), None, None).unwrap();
    let replay = direct
        .mutate("join", 1, "a", command("join", id))
        .await
        .unwrap();
    let fresh = direct
        .mutate("join", 2, "b", command("callee", id))
        .await
        .unwrap();
    for response in [replay, fresh] {
        let response: CallJoinResponse = serde_json::from_value(response).unwrap();
        let peer = response.peer.unwrap();
        assert_eq!(peer.credential_expires_at_ms, 0);
        assert_eq!(peer.ice_servers.len(), 1);
        assert!(peer.ice_servers[0]
            .urls
            .iter()
            .all(|u| u.starts_with("stun:")));
        assert!(peer.ice_servers[0].username.is_none());
        assert!(peer.ice_servers[0].credential.is_none());
    }
    config.peer.stun_urls.clear();
    config.peer.turn_urls.clear();
    let host_only = CallService::with_peer_secret(s.pool.clone(), config, None, None).unwrap();
    let join = host_only
        .mutate("join", 1, "a", command("host-only", id))
        .await
        .unwrap();
    assert_eq!(join["peer"]["credential_expires_at_ms"], 0);
    assert_eq!(join["peer"]["ice_servers"], json!([]));
    // The switch also suppresses old cached TURN data with no configured STUN.
    assert_eq!(
        host_only
            .mutate("join", 1, "a", command("join", id))
            .await
            .unwrap()["peer"]["ice_servers"],
        json!([])
    );
}

#[tokio::test]
async fn join_rejects_other_ringing_invitation_from_legacy_state() {
    let (_db, s) = setup("call_legacy_busy").await;
    let first = s.invite(1, "a", invite("first", 100)).await.unwrap();
    let id = call_id(&first);
    // Simulate pre-upgrade overlapping invitations, which new invites prohibit.
    sqlx::query("UPDATE privchat_calls SET state='ended' WHERE call_id=$1")
        .bind(id as i64).execute(&s.pool).await.unwrap();
    s.invite(3, "c", invite("second", 101)).await.unwrap();
    sqlx::query("UPDATE privchat_calls SET state='ringing' WHERE call_id=$1")
        .bind(id as i64).execute(&s.pool).await.unwrap();
    assert!(s.mutate("join", 2, "b", command("join", id)).await.is_err());
}

#[tokio::test]
async fn ringing_participants_are_atomically_busy_across_calls() {
    let (_db, s) = setup("call_ringing_busy").await;
    let (a, c) = tokio::join!(
        s.invite(1, "a", invite("a-to-b", 100)),
        s.invite(3, "c", invite("c-to-b", 101))
    );
    assert_ne!(a.is_ok(), c.is_ok(), "only one caller can reserve B");
    let (owner, channel, device, result) = match a {
        Ok(value) => (1, 100, "a", value),
        Err(_) => (3, 101, "c", c.unwrap()),
    };
    let id = call_id(&result);
    assert!(s.invite(owner, device, invite("caller-busy", channel)).await.is_err());
    assert!(s.invite(2, "b", invite("callee-busy", channel)).await.is_err());
    assert_eq!(s.active(2).await.unwrap().calls.len(), 1);
    s.mutate("reject", 2, "b", command("reject", id)).await.unwrap();
    let next = s.invite(owner, device, invite("after-reject", channel)).await.unwrap();
    assert_ne!(call_id(&next), id, "terminal/rejected participants release occupancy");
}

#[tokio::test]
async fn durable_commands_and_first_device_wins() {
    let (_db, s) = setup("call_race").await;
    let req = invite("invite", 100);
    let (a, b) = tokio::join!(s.invite(1, "a", req.clone()), s.invite(1, "a", req));
    let first = a.unwrap();
    assert_eq!(first, b.unwrap());
    let id = call_id(&first);
    let mut mismatch = invite("invite", 100);
    mismatch.media_kind = CallMediaKind::Video;
    assert!(s.invite(1, "a", mismatch).await.is_err());
    assert_eq!(
        s.active(2).await.unwrap().calls.len(),
        1,
        "offline invitation must be reconcilable"
    );
    assert!(s.active(3).await.unwrap().calls.is_empty());
    let (a, b) = tokio::join!(
        s.mutate("join", 2, "phone", command("join", id)),
        s.mutate("join", 2, "desktop", command("join", id))
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (winner, loser, response) = if let Ok(a) = a {
        ("phone", "desktop", a)
    } else {
        ("desktop", "phone", b.unwrap())
    };
    let restarted = CallService::with_peer_secret(
        s.pool.clone(),
        s.config.clone(),
        Some((
            "test-key".into(),
            "test-secret-at-least-thirty-two-bytes".into(),
        )),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    assert_eq!(
        response,
        restarted
            .mutate("join", 2, winner, command("join", id))
            .await
            .unwrap()
    );
    assert!(s
        .mutate("heartbeat", 2, loser, command("heart", id))
        .await
        .is_err());
    assert!(s.mutate("end", 2, loser, command("end", id)).await.is_err());
    let beat = s
        .mutate("heartbeat", 2, winner, command("heart", id))
        .await
        .unwrap();
    assert_eq!(
        beat,
        s.mutate("heartbeat", 2, winner, command("heart", id))
            .await
            .unwrap()
    );
    let ended = s
        .mutate("end", 2, winner, command("end", id))
        .await
        .unwrap();
    assert_eq!(
        ended,
        s.mutate("end", 2, winner, command("end", id))
            .await
            .unwrap()
    );
    assert!(
        s.mutate("join", 2, winner, command("join", id))
            .await
            .is_err(),
        "replay must not disclose stale credentials after end"
    );
    assert_eq!(s.get(1, id).await.unwrap().state, CallState::Ended);
    assert!(s.active(2).await.unwrap().calls.is_empty());
    let effects: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind='history'",
    )
    .bind(id as i64)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(effects, 1);
    let clean: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind='delete_room'",
    )
    .bind(id as i64)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(clean, 0);
}

#[tokio::test]
async fn permissions_limits_blacklists_and_reconciliation() {
    let (_db, s) = setup("call_auth").await;
    assert!(s.invite(3, "x", invite("bad", 100)).await.is_err());
    let mut request = invite("limit", 200);
    assert!(
        s.invite(1, "a", request.clone()).await.is_err(),
        "17 members must not silently truncate at 16"
    );
    request.invitee_ids = vec![2, 3];
    let value = s.invite(1, "a", request).await.unwrap();
    let id = call_id(&value);
    assert!(
        s.get(4, id).await.is_err(),
        "membership alone cannot reveal uninvited call"
    );
    sqlx::query("INSERT INTO privchat_blacklist(user_id,blocked_user_id) VALUES(2,1)")
        .execute(&s.pool)
        .await
        .unwrap();
    assert!(s
        .mutate("join", 2, "b", command("blocked", id))
        .await
        .is_err());
    sqlx::query("DELETE FROM privchat_blacklist")
        .execute(&s.pool)
        .await
        .unwrap();
    s.mutate("join", 2, "b", command("join", id)).await.unwrap();
    assert!(s.mutate("end", 2, "b", command("end", id)).await.is_err());
    sqlx::query("UPDATE privchat_group_members SET left_at=1 WHERE group_id=200 AND user_id=2")
        .execute(&s.pool)
        .await
        .unwrap();
    assert!(s
        .mutate("heartbeat", 2, "b", command("heart", id))
        .await
        .is_err());
    assert!(s.get(2, id).await.is_err());
    assert!(s.active(2).await.unwrap().calls.is_empty());
    s.sweep().await.unwrap();
    assert_eq!(
        s.get(1, id)
            .await
            .unwrap()
            .participants
            .iter()
            .find(|p| p.user_id == 2)
            .unwrap()
            .state,
        CallParticipantState::Left
    );
    let mut bad = invite("target", 100);
    bad.invitee_ids = vec![20];
    assert!(s.invite(1, "a", bad).await.is_err());
}

#[tokio::test]
async fn timeouts_and_lease_expiry_survive_restart() {
    let (_db, s) = setup("call_lease").await;
    let value = s.invite(1, "a", invite("invite", 100)).await.unwrap();
    let id = call_id(&value);
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{call,expires_at_ms}','0') WHERE call_id=$1").bind(id as i64).execute(&s.pool).await.unwrap();
    assert_eq!(
        s.get(2, id).await.unwrap().end_reason,
        Some(CallEndReason::Timeout)
    );
    let value = s.invite(1, "a", invite("again", 100)).await.unwrap();
    let id = call_id(&value);
    s.mutate("join", 2, "b", command("join", id)).await.unwrap();
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{claims,2,lease_until}','0') WHERE call_id=$1").bind(id as i64).execute(&s.pool).await.unwrap();
    s.sweep().await.unwrap();
    s.sweep().await.unwrap();
    assert_eq!(
        s.get(1, id).await.unwrap().end_reason,
        Some(CallEndReason::Timeout)
    );
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind='history'",
    )
    .bind(id as i64)
    .fetch_one(&s.pool)
    .await
    .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn join_replay_rechecks_peer_lease_and_commits_terminal_effects() {
    let (_db, s) = setup("call_replay_peer_expiry").await;
    let id = call_id(&s.invite(1, "a", invite("invite", 100)).await.unwrap());
    s.mutate("join", 1, "a", command("caller-join", id)).await.unwrap();
    s.mutate("join", 2, "b", command("callee-join", id)).await.unwrap();
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{claims,1,lease_until}','0') WHERE call_id=$1")
        .bind(id as i64).execute(&s.pool).await.unwrap();
    assert!(s.mutate("join", 2, "b", command("callee-join", id)).await.is_err(),
        "a live callee lease cannot authorize replay when its peer has expired");
    let state: String = sqlx::query_scalar("SELECT state FROM privchat_calls WHERE call_id=$1")
        .bind(id as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(state, "ended", "expiry must commit even when replay is rejected");
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind='history'")
        .bind(id as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(count, 1);
    assert!(s.active(2).await.unwrap().calls.is_empty());
    assert!(s.invite(1, "a", invite("next", 100)).await.is_ok(), "terminal replay releases occupancy");
}

#[tokio::test]
async fn join_waiting_on_occupancy_lock_cannot_cross_ringing_deadline() {
    let (_db, s) = setup("call_join_deadline_lock").await;
    let id = call_id(&s.invite(1, "a", invite("invite", 100)).await.unwrap());
    let deadline = chrono::Utc::now().timestamp_millis() + 1500;
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{call,expires_at_ms}',to_jsonb($2::bigint)) WHERE call_id=$1")
        .bind(id as i64).bind(deadline).execute(&s.pool).await.unwrap();
    let mut lock = s.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('call-user:2',0))")
        .execute(&mut *lock).await.unwrap();
    let service = s.clone();
    let join = tokio::spawn(async move { service.mutate("join", 2, "b", command("join", id)).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%pg_advisory_xact_lock%')")
                .fetch_one(&s.pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("join must reach the actual occupancy lock before expiry");
    tokio::time::sleep(std::time::Duration::from_millis((deadline - chrono::Utc::now().timestamp_millis()).max(0) as u64 + 20)).await;
    lock.rollback().await.unwrap();
    assert!(join.await.unwrap().is_err(), "lock waits cannot authorize acceptance after the absolute deadline");
    assert_eq!(s.get(1, id).await.unwrap().end_reason, Some(CallEndReason::Timeout));
}

#[tokio::test]
async fn active_group_expires_only_unanswered_invite_and_preserves_joined_authority() {
    let (_db, s) = setup("call_group_invite_expiry").await;
    let id = call_id(&s.invite(1, "a", CallInviteRequest {
        invitee_ids: vec![2, 3], ..invite("group", 200)
    }).await.unwrap());
    s.mutate("join", 1, "a", command("caller-join", id)).await.unwrap();
    s.mutate("join", 2, "b", command("callee-join", id)).await.unwrap();
    let before = s.get(1, id).await.unwrap();
    assert_eq!(before.state, CallState::Active);
    assert!(s.invite(3, "c", CallInviteRequest {
        invitee_ids: vec![4], ..invite("busy-before-expiry", 200)
    }).await.is_err());
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{call,expires_at_ms}','0') WHERE call_id=$1")
        .bind(id as i64).execute(&s.pool).await.unwrap();
    let late = command("late-join", id);
    assert!(s.mutate("join", 3, "c", late.clone()).await.is_err());
    let persisted: Value = sqlx::query_scalar("SELECT document FROM privchat_calls WHERE call_id=$1")
        .bind(id as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(persisted["call"]["state"], "active");
    assert_eq!(persisted["call"]["version"], before.version + 1);
    assert_eq!(persisted["call"]["participants"], json!([
        {"user_id":1,"state":"joined"}, {"user_id":2,"state":"joined"}, {"user_id":3,"state":"left"}
    ]));
    assert!(persisted["claims"].get("3").is_none());
    let restarted = CallService::with_peer_secret(s.pool.clone(), s.config.clone(),
        Some(("test-key".into(), "test-secret-at-least-thirty-two-bytes".into())),
        Some("isolated-turn-secret-not-production".into())).unwrap();
    restarted.sweep().await.unwrap();
    assert!(restarted.mutate("join", 3, "c", late).await.is_err());
    assert!(restarted.mutate("heartbeat", 3, "c", command("late-heartbeat", id)).await.is_err());
    assert!(restarted.active(3).await.unwrap().calls.is_empty());
    assert_eq!(restarted.get(1, id).await.unwrap().version, before.version + 1,
        "restart and repeated rejection must not duplicate the expiry transition");
    let notification_count: i64 = sqlx::query_scalar("SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind='state' AND version=$2")
        .bind(id as i64).bind((before.version + 1) as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(notification_count, 1);
    let cleanup_count: i64 = sqlx::query_scalar("SELECT count(*) FROM privchat_call_effects WHERE call_id=$1 AND kind IN ('history','delete_room','remove_participant')")
        .bind(id as i64).fetch_one(&s.pool).await.unwrap();
    assert_eq!(cleanup_count, 0, "unanswered expiry must not remove joined SFU participants or end the room");
    for (user, device, replay) in [(1, "a", "caller-join"), (2, "b", "callee-join")] {
        assert!(restarted.mutate("join", user, device, command(replay, id)).await.is_ok());
        let heartbeat = restarted.mutate("heartbeat", user, device, command("heartbeat", id)).await.unwrap();
        assert_eq!(heartbeat["call"]["state"], "active");
        let renewed = restarted.mutate("join", user, device, command("renew-media", id)).await.unwrap();
        let claims = jsonwebtoken::decode::<Value>(renewed["token"].as_str().unwrap(),
            &jsonwebtoken::DecodingKey::from_secret(b"test-secret-at-least-thirty-two-bytes"),
            &jsonwebtoken::Validation::default()).unwrap().claims;
        assert_eq!(claims["video"]["room"], format!("pc-200-{id}"));
        assert_eq!(claims["video"]["roomJoin"], true);
        assert_eq!(claims["video"]["canPublish"], true);
        assert_eq!(restarted.active(user).await.unwrap().calls.len(), 1);
        assert!(restarted.invite(user, device, CallInviteRequest {
            invitee_ids: vec![5], ..invite("still-busy", 200)
        }).await.is_err(), "joined UID must retain occupancy");
    }
    assert!(restarted.invite(3, "c", CallInviteRequest {
        invitee_ids: vec![4], ..invite("free-after-expiry", 200)
    }).await.is_ok(), "only the expired invitee can start a different call");
}

#[tokio::test]
async fn active_group_join_waiting_on_occupancy_lock_cannot_cross_invitation_deadline() {
    let (_db, s) = setup("call_group_deadline_lock").await;
    let id = call_id(&s.invite(1, "a", CallInviteRequest {
        invitee_ids: vec![2, 3], ..invite("group", 200)
    }).await.unwrap());
    s.mutate("join", 2, "b", command("join", id)).await.unwrap();
    let deadline = chrono::Utc::now().timestamp_millis() + 1500;
    sqlx::query("UPDATE privchat_calls SET document=jsonb_set(document,'{call,expires_at_ms}',to_jsonb($2::bigint)) WHERE call_id=$1")
        .bind(id as i64).bind(deadline).execute(&s.pool).await.unwrap();
    let mut lock = s.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('call-user:3',0))")
        .execute(&mut *lock).await.unwrap();
    let service = s.clone();
    let join = tokio::spawn(async move { service.mutate("join", 3, "c", command("late-join", id)).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE '%pg_advisory_xact_lock%')")
                .fetch_one(&s.pool).await.unwrap();
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("group invitee must reach actual occupancy lock before expiry");
    tokio::time::sleep(std::time::Duration::from_millis((deadline - chrono::Utc::now().timestamp_millis()).max(0) as u64 + 20)).await;
    lock.rollback().await.unwrap();
    assert!(join.await.unwrap().is_err());
    let current = s.get(1, id).await.unwrap();
    assert_eq!(current.state, CallState::Active);
    assert_eq!(current.participants.iter().find(|p| p.user_id == 3).unwrap().state, CallParticipantState::Left);
    assert_eq!(current.participants.iter().filter(|p| p.state == CallParticipantState::Joined).count(), 2);
}

#[tokio::test]
async fn token_grants_and_disabled_default() {
    let (_db, s) = setup("call_grants").await;
    let disabled = CallService::new(s.pool.clone(), CallConfig::default(), None).unwrap();
    assert!(!disabled.capabilities().enabled);
    assert!(disabled
        .invite(1, "a", invite("disabled", 100))
        .await
        .is_err());
    let value = s
        .invite(
            1,
            "a",
            CallInviteRequest {
                invitee_ids: vec![2],
                ..invite("invite", 200)
            },
        )
        .await
        .unwrap();
    let id = call_id(&value);
    let joined = s.mutate("join", 1, "a", command("join", id)).await.unwrap();
    let claims = jsonwebtoken::decode::<Value>(
        joined["token"].as_str().unwrap(),
        &jsonwebtoken::DecodingKey::from_secret(b"test-secret-at-least-thirty-two-bytes"),
        &jsonwebtoken::Validation::default(),
    )
    .unwrap()
    .claims;
    assert_eq!(claims["video"]["canPublishSources"], json!(["microphone"]));
    assert_eq!(claims["video"]["room"], format!("pc-200-{id}"));
    assert_eq!(claims["video"]["canPublishData"], false);
    assert!(claims["video"].get("roomAdmin").is_none());
    assert!(claims["exp"].as_i64().unwrap() - chrono::Utc::now().timestamp() <= 60);
}

#[tokio::test]
#[ignore = "requires parent-owned local LiveKit fixture; run explicitly"]
async fn livekit_real_join_and_delete_room() {
    let (_db, mut s) = setup("call_livekit").await;
    let path = std::env::var("PRIVCHAT_CALL_LIVEKIT_FIXTURE").expect("fixture path required");
    let config = std::fs::read_to_string(path).expect("read test SFU fixture");
    let line = config
        .lines()
        .skip_while(|line| line.trim() != "keys:")
        .nth(1)
        .expect("test key mapping");
    let (key, secret) = line.trim().split_once(':').expect("test key mapping");
    let port: u16 = config
        .lines()
        .find_map(|line| line.strip_prefix("port:"))
        .expect("fixture port")
        .trim()
        .parse()
        .unwrap();
    let mut call_config = s.config.clone();
    call_config.livekit_url = format!("ws://127.0.0.1:{port}");
    s = CallService::with_peer_secret(
        s.pool.clone(),
        call_config,
        Some((
            key.trim().trim_matches(['\"', '\'']).into(),
            secret.trim().trim_matches(['\"', '\'']).into(),
        )),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    let value = s
        .invite(
            1,
            "a",
            CallInviteRequest {
                invitee_ids: vec![2],
                ..invite("invite", 200)
            },
        )
        .await
        .unwrap();
    let id = call_id(&value);
    let response = s.mutate("join", 1, "a", command("join", id)).await.unwrap();
    assert_eq!(response["call"]["transport"], "livekit");
    assert!(response["peer"].is_null());
    let mut url = url::Url::parse(&format!("http://127.0.0.1:{port}/rtc")).unwrap();
    url.query_pairs_mut()
        .append_pair("access_token", response["token"].as_str().unwrap())
        .append_pair("auto_subscribe", "1")
        .append_pair("protocol", "15")
        .append_pair("sdk", "rust");
    let upgrade = reqwest::Client::new()
        .get(url)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .unwrap_or_else(|_| panic!("LiveKit handshake transport failed (URL redacted)"));
    assert_eq!(
        upgrade.status(),
        101,
        "signed call credentials must authenticate with real SFU"
    );
    let connection = upgrade.upgrade().await.unwrap();
    let call = s.get(1, id).await.unwrap();
    s.media_cleanup(&call, None).await.unwrap();
    s.media_cleanup(&call, None).await.unwrap();
    drop(connection);
}

#[tokio::test]
async fn reject_cancel_group_leave_and_heartbeat_capability() {
    let (_db, s) = setup("call_transitions").await;
    let old: CallCapabilitiesResponse=serde_json::from_value(json!({"enabled":true,"audio_participant_limit":16,"video_participant_limit":9,"ringing_timeout_seconds":45})).unwrap();
    assert_eq!(old.heartbeat_lease_seconds, 60);
    assert_eq!(s.capabilities().heartbeat_lease_seconds, 60);
    let mut cfg = s.config.clone();
    cfg.heartbeat_lease_seconds = 10;
    let short = CallService::with_peer_secret(
        s.pool.clone(),
        cfg,
        Some(("key".into(), "secret".into())),
        Some("isolated-turn-secret-not-production".into()),
    )
    .unwrap();
    assert_eq!(short.capabilities().heartbeat_lease_seconds, 10);
    let id = call_id(
        &s.invite(1, "a", invite("reject-invite", 100))
            .await
            .unwrap(),
    );
    let rejected = s
        .mutate("reject", 2, "b", command("reject", id))
        .await
        .unwrap();
    assert_eq!(rejected["call"]["end_reason"], "rejected");
    let id = call_id(
        &s.invite(1, "a", invite("cancel-invite", 100))
            .await
            .unwrap(),
    );
    assert!(s
        .mutate("cancel", 1, "wrong", command("cancel", id))
        .await
        .is_err());
    let cancelled = s
        .mutate("cancel", 1, "a", command("cancel", id))
        .await
        .unwrap();
    assert_eq!(cancelled["call"]["end_reason"], "cancelled");
    let mut req = invite("video-overlimit", 200);
    req.media_kind = CallMediaKind::Video;
    req.invitee_ids = (2..=10).collect();
    assert!(s.invite(1, "a", req).await.is_err());
    let mut req = invite("group", 200);
    req.invitee_ids = vec![2, 3];
    let id = call_id(&s.invite(1, "a", req).await.unwrap());
    s.mutate("join", 2, "b", command("join", id)).await.unwrap();
    let left = s
        .mutate("leave", 2, "b", command("leave", id))
        .await
        .unwrap();
    assert_eq!(left["call"]["state"], "active");
    assert!(s
        .mutate("join", 2, "b", command("rejoin", id))
        .await
        .is_err());
    let ended = s.mutate("end", 1, "a", command("end", id)).await.unwrap();
    assert_eq!(ended["call"]["state"], "ended");
}
