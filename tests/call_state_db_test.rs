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
        ..Default::default()
    };
    let service = CallService::new(
        pool,
        config,
        Some((
            "test-key".into(),
            "test-secret-at-least-thirty-two-bytes".into(),
        )),
    )
    .unwrap();
    (db, service)
}
fn invite(id: &str, channel: u64) -> CallInviteRequest {
    CallInviteRequest {
        command_id: id.into(),
        channel_id: channel,
        media_kind: CallMediaKind::Audio,
        invitee_ids: vec![],
    }
}
fn command(id: &str, call: u64) -> CallMutationRequest {
    CallMutationRequest {
        command_id: id.into(),
        call_id: call,
    }
}
fn call_id(value: &Value) -> u64 {
    value["call"]["call_id"].as_u64().unwrap()
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
    let restarted = CallService::new(
        s.pool.clone(),
        s.config.clone(),
        Some((
            "test-key".into(),
            "test-secret-at-least-thirty-two-bytes".into(),
        )),
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
    assert_eq!(clean, 1);
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
async fn token_grants_and_disabled_default() {
    let (_db, s) = setup("call_grants").await;
    let disabled = CallService::new(s.pool.clone(), CallConfig::default(), None).unwrap();
    assert!(!disabled.capabilities().enabled);
    assert!(disabled
        .invite(1, "a", invite("disabled", 100))
        .await
        .is_err());
    let value = s.invite(1, "a", invite("invite", 100)).await.unwrap();
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
    assert_eq!(claims["video"]["room"], format!("pc-100-{id}"));
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
    s = CallService::new(
        s.pool.clone(),
        s.config.clone(),
        Some((
            key.trim().trim_matches(['\"', '\'']).into(),
            secret.trim().trim_matches(['\"', '\'']).into(),
        )),
    )
    .unwrap();
    let value = s.invite(1, "a", invite("invite", 100)).await.unwrap();
    let id = call_id(&value);
    let response = s.mutate("join", 1, "a", command("join", id)).await.unwrap();
    let mut url = url::Url::parse("http://127.0.0.1:56780/rtc").unwrap();
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
    let short =
        CallService::new(s.pool.clone(), cfg, Some(("key".into(), "secret".into()))).unwrap();
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
