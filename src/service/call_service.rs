//! Durable call state. Row locks serialize devices; commands and effects commit with state.
use super::call_config::CallConfig;
use crate::rpc::error::{RpcError, RpcResult};
use privchat_protocol::rpc::call::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{BTreeMap, BTreeSet};
pub mod components;
mod peer;
mod push;
pub use push::{
    CallHttpPushProvider, CallPushNotification, CallPushProvider, CallPushResult, CallPushWorker,
};

fn db(_: sqlx::Error) -> RpcError {
    RpcError::from_code(
        privchat_protocol::ErrorCode::ServiceUnavailable,
        "Call storage unavailable".into(),
    )
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn encode<T: Serialize>(value: T) -> RpcResult<Value> {
    serde_json::to_value(value).map_err(|_| RpcError::internal("Call encoding failed"))
}
#[derive(Clone, Serialize, Deserialize)]
struct DeviceClaim {
    device: String,
    lease_until: i64,
}
#[derive(Clone, Serialize, Deserialize)]
struct StoredCall {
    #[serde(default)]
    peer_revision: u32,
    #[serde(default)]
    peer_answered: bool,
    #[serde(default)]
    peer_seq: u64,
    call: CallSnapshot,
    channel_type: i16,
    claims: BTreeMap<u64, DeviceClaim>,
}

#[derive(Clone)]
pub struct CallService {
    pub pool: PgPool,
    pub config: CallConfig,
    credentials: Option<(String, String)>,
    turn_secret: Option<String>,
}
impl CallService {
    // Keep local policy available for a later managed enable, while failing closed at startup.
    pub fn configured(pool: PgPool, config: CallConfig) -> Self {
        Self::from_environment(pool.clone(), config.clone()).unwrap_or_else(|_| {
            tracing::error!("Local call configuration unavailable; calls disabled until a valid managed apply");
            let mut config = config;
            config.enabled = false;
            Self { pool, config, credentials: None, turn_secret: None }
        })
    }
    pub fn new(
        pool: PgPool,
        config: CallConfig,
        credentials: Option<(String, String)>,
    ) -> anyhow::Result<Self> {
        let secret = if config.enabled && config.peer.enabled && config.peer.allow_turn {
            std::env::var(&config.peer.turn_secret_env).ok()
        } else {
            None
        };
        Self::with_peer_secret(pool, config, credentials, secret)
    }
    pub fn with_peer_secret(
        pool: PgPool,
        config: CallConfig,
        credentials: Option<(String, String)>,
        turn_secret: Option<String>,
    ) -> anyhow::Result<Self> {
        config.validate()?;
        if config.enabled && !config.livekit_url.is_empty() {
            anyhow::ensure!(
                credentials
                    .as_ref()
                    .is_some_and(|(k, s)| !k.is_empty() && !s.is_empty()),
                "LiveKit credentials unavailable"
            );
        }
        let turn_secret =
            turn_secret.filter(|_| config.enabled && config.peer.enabled && config.peer.allow_turn);
        if config.enabled && config.peer.enabled && config.peer.allow_turn {
            anyhow::ensure!(
                turn_secret.as_ref().is_some_and(|s| s.len() >= 16),
                "TURN secret unavailable or too short"
            );
        }
        Ok(Self {
            pool,
            config,
            credentials,
            turn_secret,
        })
    }
    pub fn from_environment(pool: PgPool, config: CallConfig) -> anyhow::Result<Self> {
        let credentials = if config.enabled {
            Some((
                std::env::var(&config.api_key_env).unwrap_or_default(),
                std::env::var(&config.api_secret_env).unwrap_or_default(),
            ))
        } else {
            None
        };
        Self::new(pool, config, credentials)
    }
    pub fn capabilities(&self) -> CallCapabilitiesResponse {
        CallCapabilitiesResponse {
            enabled: self.config.enabled,
            peer_enabled: self.config.enabled && self.config.peer.enabled,
            livekit_enabled: self.config.enabled
                && !self.config.livekit_url.is_empty()
                && self.credentials.is_some(),
            audio_participant_limit: self.config.audio_participant_limit,
            video_participant_limit: self.config.video_participant_limit,
            ringing_timeout_seconds: self.config.ringing_timeout_seconds,
            heartbeat_lease_seconds: self.config.heartbeat_lease_seconds,
        }
    }
    fn enabled(&self) -> RpcResult<()> {
        if !self.config.enabled {
            return Err(RpcError::from_code(
                privchat_protocol::ErrorCode::ServiceUnavailable,
                "Calls disabled".into(),
            ));
        }
        Ok(())
    }
    fn limit(&self, kind: CallMediaKind) -> usize {
        (match kind {
            CallMediaKind::Audio => self.config.audio_participant_limit,
            CallMediaKind::Video => self.config.video_participant_limit,
        }) as usize
    }
    async fn command(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor: u64,
        device: &str,
        id: &str,
        route: &str,
        body: &Value,
    ) -> RpcResult<(String, Option<Value>)> {
        if device.is_empty() || device.len() > 256 || id.trim().is_empty() || id.len() > 128 {
            return Err(RpcError::validation(
                "A nonempty device and command_id (max 128 bytes) are required",
            ));
        }
        // An xact advisory lock also covers a command whose row does not yet exist.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("call-command:{actor}:{device}:{id}"))
            .execute(&mut **tx)
            .await
            .map_err(db)?;
        // Preserve fingerprints of pre-peer commands when the additive flag is absent/false.
        let mut body = body.clone();
        if body.get("supports_peer").and_then(Value::as_bool) == Some(false) {
            body.as_object_mut().unwrap().remove("supports_peer");
        }
        let fingerprint = hex::encode(Sha256::digest(format!("{route}:{body}")));
        let peer_used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_call_peer_commands WHERE user_id=$1 AND device_id=$2 AND command_id=$3)")
            .bind(actor as i64).bind(device).bind(id).fetch_one(&mut **tx).await.map_err(db)?;
        if peer_used {
            return Err(RpcError::validation(
                "command_id already used by peer signalling",
            ));
        }
        let row = sqlx::query("SELECT fingerprint, response FROM privchat_call_commands WHERE user_id=$1 AND device_id=$2 AND command_id=$3")
            .bind(actor as i64).bind(device).bind(id).fetch_optional(&mut **tx).await.map_err(db)?;
        if let Some(row) = row {
            if row.get::<String, _>("fingerprint") != fingerprint {
                return Err(RpcError::validation(
                    "command_id reused with a different request",
                ));
            }
            return Ok((fingerprint, Some(row.get("response"))));
        }
        Ok((fingerprint, None))
    }
    async fn record(
        tx: &mut Transaction<'_, Postgres>,
        actor: u64,
        device: &str,
        id: &str,
        fingerprint: &str,
        response: &Value,
    ) -> RpcResult<()> {
        sqlx::query("INSERT INTO privchat_call_commands(user_id,device_id,command_id,fingerprint,response,created_at) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(actor as i64).bind(device).bind(id).bind(fingerprint).bind(response).bind(now()).execute(&mut **tx).await.map_err(db)?;
        Ok(())
    }
    // Read authoritative membership, not the in-memory channel cache. DB channel types are 0/1.
    async fn members(
        tx: &mut Transaction<'_, Postgres>,
        channel: u64,
    ) -> RpcResult<(i16, BTreeMap<u64, (i16, bool)>)> {
        let row = sqlx::query("SELECT channel_type,direct_user1_id,direct_user2_id,group_id FROM privchat_channels WHERE channel_id=$1 FOR SHARE")
            .bind(channel as i64).fetch_optional(&mut **tx).await.map_err(db)?.ok_or_else(|| RpcError::not_found("Channel unavailable"))?;
        let kind: i16 = row.get("channel_type");
        let mut members = BTreeMap::new();
        if kind == 0 {
            for name in ["direct_user1_id", "direct_user2_id"] {
                if let Some(id) = row.get::<Option<i64>, _>(name) {
                    members.insert(id as u64, (2, false));
                }
            }
        } else if kind == 1 {
            let rows = sqlx::query("SELECT m.user_id,m.role,m.mute_until,m.permissions,g.all_muted,g.allow_member_post FROM privchat_group_members m JOIN privchat_groups g ON g.group_id=m.group_id WHERE m.group_id=$1 AND m.left_at IS NULL AND g.status=0 FOR SHARE OF m,g")
                .bind(row.get::<Option<i64>,_>("group_id")).fetch_all(&mut **tx).await.map_err(db)?;
            for r in rows {
                let role = r.get::<Option<i16>, _>("role").unwrap_or(2);
                let mute = r
                    .get::<Option<i64>, _>("mute_until")
                    .is_some_and(|until| until == 0 || until > now());
                let policy = role > 1
                    && (r.get::<bool, _>("all_muted") || !r.get::<bool, _>("allow_member_post"));
                let perms = r
                    .get::<Option<Value>, _>("permissions")
                    .unwrap_or(json!({}));
                members.insert(
                    r.get::<i64, _>("user_id") as u64,
                    (
                        role,
                        mute || policy
                            || perms.get("can_send_message").and_then(Value::as_bool)
                                == Some(false),
                    ),
                );
            }
        } else {
            return Err(RpcError::validation(
                "Calls require a direct or group channel",
            ));
        }
        Ok((kind, members))
    }
    async fn not_blocked(tx: &mut Transaction<'_, Postgres>, a: u64, b: u64) -> RpcResult<()> {
        let blocked: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_blacklist WHERE (user_id=$1 AND blocked_user_id=$2) OR (user_id=$2 AND blocked_user_id=$1))")
            .bind(a as i64).bind(b as i64).fetch_one(&mut **tx).await.map_err(db)?;
        if blocked {
            return Err(RpcError::forbidden("Call blocked by contact policy"));
        }
        Ok(())
    }
    async fn load(tx: &mut Transaction<'_, Postgres>, id: u64) -> RpcResult<StoredCall> {
        let value: Value =
            sqlx::query_scalar("SELECT document FROM privchat_calls WHERE call_id=$1 FOR UPDATE")
                .bind(id as i64)
                .fetch_optional(&mut **tx)
                .await
                .map_err(db)?
                .ok_or_else(|| RpcError::not_found("Call unavailable"))?;
        serde_json::from_value(value).map_err(|_| RpcError::internal("Invalid stored call"))
    }
    async fn effect(
        tx: &mut Transaction<'_, Postgres>,
        call: &CallSnapshot,
        kind: &str,
        payload: Value,
    ) -> RpcResult<()> {
        sqlx::query("INSERT INTO privchat_call_effects(call_id,version,kind,payload) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(call.call_id as i64).bind(call.version as i64).bind(kind).bind(payload).execute(&mut **tx).await.map_err(db)?;
        Ok(())
    }
    async fn save(
        tx: &mut Transaction<'_, Postgres>,
        stored: &StoredCall,
        notify: bool,
    ) -> RpcResult<()> {
        let state = match stored.call.state {
            CallState::Ringing => "ringing",
            CallState::Active => "active",
            CallState::Ended => "ended",
        };
        sqlx::query("INSERT INTO privchat_calls(call_id,channel_id,state,document,updated_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(call_id) DO UPDATE SET state=EXCLUDED.state,document=EXCLUDED.document,updated_at=EXCLUDED.updated_at")
            .bind(stored.call.call_id as i64).bind(stored.call.channel_id as i64).bind(state).bind(encode(stored)?).bind(now()).execute(&mut **tx).await.map_err(db)?;
        push::reconcile(tx, stored).await?;
        if notify {
            Self::effect(tx, &stored.call, "state", json!({})).await?;
        }
        if stored.call.state == CallState::Ended {
            Self::effect(tx, &stored.call, "history", json!({})).await?;
            if stored.call.transport == CallTransport::Livekit {
                Self::effect(tx, &stored.call, "delete_room", json!({})).await?;
            } else {
                Self::clear_peer(tx, stored.call.call_id).await?;
            }
        }
        Ok(())
    }
    fn finish(stored: &mut StoredCall, reason: CallEndReason) {
        stored.call.state = CallState::Ended;
        stored.call.end_reason = Some(reason);
        stored.call.ended_at_ms = Some(now());
        for p in &mut stored.call.participants {
            if p.state == CallParticipantState::Joined {
                p.state = CallParticipantState::Left;
            }
        }
    }
    async fn expire(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        stored: &mut StoredCall,
    ) -> RpcResult<bool> {
        if stored.call.state == CallState::Ended {
            return Ok(false);
        }
        let mut changed = false;
        let current = now();
        let (_, members) = Self::members(tx, stored.call.channel_id).await?;
        for p in &mut stored.call.participants {
            let expired = !members.contains_key(&p.user_id)
                || (p.state == CallParticipantState::Joined
                    && stored
                        .claims
                        .get(&p.user_id)
                        .is_none_or(|c| c.lease_until <= current));
            if expired
                && matches!(
                    p.state,
                    CallParticipantState::Invited | CallParticipantState::Joined
                )
            {
                p.state = CallParticipantState::Left;
                changed = true;
            }
        }
        let joined = stored
            .call
            .participants
            .iter()
            .filter(|p| p.state == CallParticipantState::Joined)
            .count();
        if (stored.call.state == CallState::Ringing && current >= stored.call.expires_at_ms)
            || (stored.call.state == CallState::Active
                && (joined == 0 || (stored.channel_type == 0 && joined < 2)))
        {
            Self::finish(stored, CallEndReason::Timeout);
            changed = true;
        }
        if changed {
            stored.call.version += 1;
            for p in &stored.call.participants {
                if p.state == CallParticipantState::Left
                    && stored.call.transport == CallTransport::Livekit
                {
                    if let Some(c) = stored.claims.get(&p.user_id) {
                        Self::effect(tx, &stored.call, "remove_participant", json!({"identity": self.identity(stored.call.call_id,p.user_id,&c.device)})).await?;
                    }
                }
            }
            Self::save(tx, stored, true).await?;
        }
        Ok(changed)
    }
    pub async fn invite(
        &self,
        actor: u64,
        device: &str,
        req: CallInviteRequest,
    ) -> RpcResult<Value> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let runtime = self.runtime_in(&mut tx).await?;
        let service = &runtime;
        service.enabled()?;
        let (fingerprint, replay) = service
            .command(
                &mut tx,
                actor,
                device,
                &req.command_id,
                "invite",
                &encode(&req)?,
            )
            .await?;
        let (kind, members) = Self::members(&mut tx, req.channel_id).await?;
        let (_, muted) = members
            .get(&actor)
            .ok_or_else(|| RpcError::forbidden("Not a channel member"))?;
        if *muted {
            return Err(RpcError::forbidden("Member cannot initiate calls"));
        }
        if let Some(value) = replay {
            return Ok(value);
        }
        if kind == 0 && !service.config.peer.enabled {
            return Err(RpcError::from_code(
                privchat_protocol::ErrorCode::ServiceUnavailable,
                "Direct peer calls unavailable; peer transport must be enabled".into(),
            ));
        }
        let transport = if kind == 0 {
            CallTransport::Webrtc
        } else {
            CallTransport::Livekit
        };
        if transport == CallTransport::Webrtc && !req.supports_peer {
            return Err(RpcError::validation(
                "This direct call requires a peer-capable client",
            ));
        }
        if transport == CallTransport::Livekit && service.config.livekit_url.is_empty() {
            return Err(RpcError::validation("LiveKit is unavailable for this call"));
        }
        let mut targets: BTreeSet<u64> = if req.invitee_ids.is_empty() {
            members.keys().copied().collect()
        } else {
            req.invitee_ids.iter().copied().collect()
        };
        targets.remove(&actor);
        if targets.is_empty() || targets.len() + 1 > service.limit(req.media_kind) {
            return Err(RpcError::validation(
                "Invitee count exceeds call limit or is empty",
            ));
        }
        for &target in &targets {
            if !members.contains_key(&target) {
                return Err(RpcError::forbidden("Invitee is not a channel member"));
            }
            Self::not_blocked(&mut tx, actor, target).await?;
        }
        // Cross-call busy arbitration uses a deterministic per-user lock order.
        let all: BTreeSet<_> = targets.iter().copied().chain([actor]).collect();
        for user in &all {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("call-user:{user}"))
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_calls WHERE state <> 'ended' AND (document->'call'->'participants' @> $1::jsonb OR document->'call'->'participants' @> $2::jsonb))")
                .bind(json!([{"user_id":user,"state":"joined"}])).bind(json!([{"user_id":user,"state":"invited"}])).fetch_one(&mut *tx).await.map_err(db)?;
            if busy {
                return Err(RpcError::validation("User is busy in another call"));
            }
        }
        let id = crate::infra::next_message_id();
        let timestamp = now();
        let participants = all
            .iter()
            .map(|&user_id| CallParticipant {
                user_id,
                state: if user_id == actor {
                    CallParticipantState::Joined
                } else {
                    CallParticipantState::Invited
                },
            })
            .collect();
        let stored = StoredCall {
            peer_revision: 0,
            peer_answered: false,
            peer_seq: 0,
            call: CallSnapshot {
                transport,
                call_id: id,
                channel_id: req.channel_id,
                initiator_id: actor,
                media_kind: req.media_kind,
                state: CallState::Ringing,
                end_reason: None,
                version: 1,
                created_at_ms: timestamp,
                answered_at_ms: None,
                ended_at_ms: None,
                expires_at_ms: timestamp + i64::from(service.config.ringing_timeout_seconds) * 1000,
                participants,
            },
            channel_type: kind,
            claims: BTreeMap::from([(
                actor,
                DeviceClaim {
                    device: device.into(),
                    lease_until: timestamp + i64::from(service.config.heartbeat_lease_seconds) * 1000,
                },
            )]),
        };
        Self::save(&mut tx, &stored, true).await?;
        let response = encode(CallResponse { call: stored.call })?;
        Self::record(
            &mut tx,
            actor,
            device,
            &req.command_id,
            &fingerprint,
            &response,
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(response)
    }
    pub async fn mutate(
        &self,
        route: &str,
        actor: u64,
        device: &str,
        req: CallMutationRequest,
    ) -> RpcResult<Value> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let runtime = self.runtime_in(&mut tx).await?;
        let service = &runtime;
        service.enabled()?;
        let (fingerprint, replay) = service
            .command(
                &mut tx,
                actor,
                device,
                &req.command_id,
                route,
                &encode(&req)?,
            )
            .await?;
        let mut stored = Self::load(&mut tx, req.call_id).await?;
        let (_, members) = Self::members(&mut tx, stored.call.channel_id).await?;
        let &(role, muted) = members
            .get(&actor)
            .ok_or_else(|| RpcError::forbidden("Not a channel member"))?;
        let position = stored
            .call
            .participants
            .iter()
            .position(|p| p.user_id == actor)
            .ok_or_else(|| RpcError::forbidden("Not invited to call"))?;
        if route == "join" && stored.call.transport == CallTransport::Webrtc && !req.supports_peer {
            return Err(RpcError::validation(
                "This direct call requires a peer-capable client",
            ));
        }
        if let Some(mut response) = replay {
            if route == "join" {
                if stored.call.state == CallState::Ended
                    || (stored.call.state == CallState::Ringing
                        && stored.call.expires_at_ms <= now())
                    || muted
                    || stored
                        .claims
                        .get(&actor)
                        .is_none_or(|c| c.device != device || c.lease_until <= now())
                    || stored.call.participants[position].state != CallParticipantState::Joined
                {
                    return Err(RpcError::forbidden("Call credential no longer authorized"));
                }
                for peer in &stored.call.participants {
                    if peer.user_id != actor && peer.state == CallParticipantState::Joined {
                        Self::not_blocked(&mut tx, actor, peer.user_id).await?;
                    }
                }
                if stored.call.transport == CallTransport::Webrtc && !service.config.peer.allow_turn {
                    // A policy change must also suppress credentials cached by an older command.
                    response["peer"] = encode(service.peer_config(&stored.call, actor)?)?;
                }
            }
            return Ok(response);
        }
        service.expire(&mut tx, &mut stored).await?;
        if stored.call.state == CallState::Ended {
            tx.commit().await.map_err(db)?;
            return Err(RpcError::validation(
                "Call has ended; reconcile using call/get",
            ));
        }
        let participant = &stored.call.participants[position];
        let owns = stored
            .claims
            .get(&actor)
            .is_some_and(|c| c.device == device);
        let claimed_elsewhere = stored
            .claims
            .get(&actor)
            .is_some_and(|c| c.device != device);
        let mut notify = true;
        match route {
            "join" => {
                if muted {
                    return Err(RpcError::forbidden("Member cannot join calls"));
                }
                if claimed_elsewhere {
                    return Err(RpcError::forbidden("Call accepted on another device"));
                }
                if matches!(
                    participant.state,
                    CallParticipantState::Left | CallParticipantState::Rejected
                ) {
                    return Err(RpcError::forbidden("Participant already left or rejected"));
                }
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                    .bind(format!("call-user:{actor}"))
                    .execute(&mut *tx)
                    .await
                    .map_err(db)?;
                let busy: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_calls WHERE call_id <> $1 AND state <> 'ended' AND (document->'call'->'participants' @> $2::jsonb OR document->'call'->'participants' @> $3::jsonb))")
                    .bind(req.call_id as i64).bind(json!([{"user_id":actor,"state":"joined"}])).bind(json!([{"user_id":actor,"state":"invited"}])).fetch_one(&mut *tx).await.map_err(db)?;
                if busy {
                    return Err(RpcError::validation("User is busy in another call"));
                }
                for peer in &stored.call.participants {
                    if peer.user_id != actor
                        && matches!(
                            peer.state,
                            CallParticipantState::Joined | CallParticipantState::Invited
                        )
                    {
                        Self::not_blocked(&mut tx, actor, peer.user_id).await?;
                    }
                }
                stored.call.participants[position].state = CallParticipantState::Joined;
                stored.claims.insert(
                    actor,
                    DeviceClaim {
                        device: device.into(),
                        lease_until: now() + i64::from(service.config.heartbeat_lease_seconds) * 1000,
                    },
                );
                if actor != stored.call.initiator_id && stored.call.state == CallState::Ringing {
                    stored.call.state = CallState::Active;
                    stored.call.answered_at_ms = Some(now());
                }
            }
            "heartbeat" => {
                if !owns || participant.state != CallParticipantState::Joined {
                    return Err(RpcError::forbidden(
                        "Only the accepting device can heartbeat",
                    ));
                }
                stored.claims.get_mut(&actor).unwrap().lease_until =
                    now() + i64::from(service.config.heartbeat_lease_seconds) * 1000;
                notify = false;
            }
            "reject" => {
                if actor == stored.call.initiator_id
                    || participant.state != CallParticipantState::Invited
                {
                    return Err(RpcError::validation(
                        "Only an unanswered invite can be rejected",
                    ));
                }
                stored.call.participants[position].state = CallParticipantState::Rejected;
                if stored.channel_type == 0
                    || !stored
                        .call
                        .participants
                        .iter()
                        .any(|p| p.state == CallParticipantState::Invited)
                        && stored.call.state == CallState::Ringing
                {
                    Self::finish(&mut stored, CallEndReason::Rejected);
                }
            }
            "cancel" => {
                if actor != stored.call.initiator_id
                    || !owns
                    || stored.call.state != CallState::Ringing
                {
                    return Err(RpcError::forbidden(
                        "Only the initiating device can cancel ringing",
                    ));
                }
                Self::finish(&mut stored, CallEndReason::Cancelled);
            }
            "leave" | "end" => {
                if !owns || participant.state != CallParticipantState::Joined {
                    return Err(RpcError::forbidden(
                        "Only the accepting device may leave or end",
                    ));
                }
                if route == "end"
                    && stored.channel_type == 1
                    && actor != stored.call.initiator_id
                    && role > 1
                {
                    return Err(RpcError::forbidden(
                        "Only initiator or group administrator may end",
                    ));
                }
                stored.call.participants[position].state = CallParticipantState::Left;
                if route == "end"
                    || stored.channel_type == 0
                    || !stored
                        .call
                        .participants
                        .iter()
                        .any(|p| p.state == CallParticipantState::Joined)
                {
                    Self::finish(&mut stored, CallEndReason::Normal);
                } else {
                    Self::effect(
                        &mut tx,
                        &stored.call,
                        "remove_participant",
                        json!({"identity":service.identity(stored.call.call_id,actor,device)}),
                    )
                    .await?;
                }
            }
            _ => return Err(RpcError::validation("Unknown call operation")),
        }
        if notify {
            stored.call.version += 1;
        }
        let response = if route == "join" {
            let expires = now() + i64::from(service.config.token_ttl_seconds) * 1000;
            let peer = if stored.call.transport == CallTransport::Webrtc {
                Some(service.peer_config(&stored.call, actor)?)
            } else {
                None
            };
            encode(CallJoinResponse {
                call: stored.call.clone(),
                url: if peer.is_none() {
                    service.config.livekit_url.clone()
                } else {
                    String::new()
                },
                token: if peer.is_none() {
                    service.join_token(&stored.call, actor, device, expires)?
                } else {
                    String::new()
                },
                expires_at_ms: if peer.is_none() { expires } else { 0 },
                peer,
            })?
        } else {
            encode(CallResponse {
                call: stored.call.clone(),
            })?
        };
        Self::save(&mut tx, &stored, notify).await?;
        Self::record(
            &mut tx,
            actor,
            device,
            &req.command_id,
            &fingerprint,
            &response,
        )
        .await?;
        tx.commit().await.map_err(db)?;
        Ok(response)
    }
    pub async fn get(&self, actor: u64, id: u64) -> RpcResult<CallSnapshot> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let runtime = self.runtime_in(&mut tx).await?;
        let service = &runtime;
        service.enabled()?;
        let mut stored = Self::load(&mut tx, id).await?;
        let (_, members) = Self::members(&mut tx, stored.call.channel_id).await?;
        if !members.contains_key(&actor)
            || !stored.call.participants.iter().any(|p| p.user_id == actor)
        {
            return Err(RpcError::forbidden("Call unavailable to this user"));
        }
        service.expire(&mut tx, &mut stored).await?;
        tx.commit().await.map_err(db)?;
        Ok(stored.call)
    }
    pub async fn active(&self, actor: u64) -> RpcResult<CallActiveResponse> {
        self.enabled()?;
        let ids: Vec<i64> = sqlx::query_scalar("SELECT call_id FROM privchat_calls WHERE state <> 'ended' AND document->'call'->'participants' @> $1::jsonb ORDER BY call_id")
            .bind(json!([{"user_id":actor}])).fetch_all(&self.pool).await.map_err(db)?;
        let mut calls = Vec::new();
        for id in ids {
            match self.get(actor, id as u64).await {
                Ok(call)
                    if call.state != CallState::Ended
                        && call.participants.iter().any(|p| {
                            p.user_id == actor
                                && matches!(
                                    p.state,
                                    CallParticipantState::Invited | CallParticipantState::Joined
                                )
                        }) =>
                {
                    calls.push(call)
                }
                Ok(_) => (),
                Err(e) if e.code == privchat_protocol::ErrorCode::PermissionDenied => (),
                Err(e) => return Err(e),
            }
        }
        Ok(CallActiveResponse { calls })
    }
    pub async fn sweep(&self) -> RpcResult<()> {
        self.sweep_peer().await?;
        let ids: Vec<i64> = sqlx::query_scalar("SELECT call_id FROM privchat_calls WHERE state <> 'ended' ORDER BY updated_at LIMIT 100")
            .fetch_all(&self.pool).await.map_err(db)?;
        for id in ids {
            let mut tx = self.pool.begin().await.map_err(db)?;
            let mut stored = Self::load(&mut tx, id as u64).await?;
            self.expire(&mut tx, &mut stored).await?;
            // Rotate even healthy calls so a large active population cannot starve expiry.
            sqlx::query("UPDATE privchat_calls SET updated_at=$2 WHERE call_id=$1")
                .bind(id)
                .bind(now())
                .execute(&mut *tx)
                .await
                .map_err(db)?;
            tx.commit().await.map_err(db)?;
        }
        Ok(())
    }
    pub fn room(call: &CallSnapshot) -> String {
        format!("pc-{}-{}", call.channel_id, call.call_id)
    }
    fn identity(&self, call: u64, actor: u64, device: &str) -> String {
        hex::encode(Sha256::digest(format!("{call}:{actor}:{device}")))
    }
    fn token(&self, claims: Value) -> RpcResult<String> {
        let (_, secret) = self
            .credentials
            .as_ref()
            .ok_or_else(|| RpcError::internal("LiveKit unavailable"))?;
        jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .map_err(|_| RpcError::internal("Call credential generation failed"))
    }
    pub fn join_token(
        &self,
        call: &CallSnapshot,
        actor: u64,
        device: &str,
        expires: i64,
    ) -> RpcResult<String> {
        if call.transport != CallTransport::Livekit {
            return Err(RpcError::forbidden(
                "Peer calls do not receive SFU credentials",
            ));
        }
        let (key, _) = self
            .credentials
            .as_ref()
            .ok_or_else(|| RpcError::internal("LiveKit unavailable"))?;
        let sources = if call.media_kind == CallMediaKind::Audio {
            vec!["microphone"]
        } else {
            vec!["microphone", "camera"]
        };
        self.token(json!({"iss":key,"sub":self.identity(call.call_id,actor,device),"nbf":now()/1000-5,"exp":expires/1000,
            "video":{"roomJoin":true,"room":Self::room(call),"canSubscribe":true,"canPublish":true,"canPublishData":false,"canPublishSources":sources}}))
    }
    pub async fn media_cleanup(
        &self,
        call: &CallSnapshot,
        identity: Option<&str>,
    ) -> RpcResult<()> {
        if call.transport == CallTransport::Webrtc {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let runtime = self.runtime_in(&mut tx).await?;
        let service = &runtime;
        let (key, _) = service
            .credentials
            .as_ref()
            .ok_or_else(|| RpcError::internal("LiveKit unavailable"))?;
        let grant = if identity.is_some() {
            json!({"roomAdmin":true,"room":Self::room(call)})
        } else {
            json!({"roomCreate":true})
        };
        let token =
            service.token(json!({"iss":key,"nbf":now()/1000-5,"exp":now()/1000+60,"video":grant}))?;
        let method = if identity.is_some() {
            "RemoveParticipant"
        } else {
            "DeleteRoom"
        };
        let mut base = url::Url::parse(&service.config.livekit_url)
            .map_err(|_| RpcError::internal("Invalid LiveKit URL"))?;
        let scheme = if base.scheme() == "wss" {
            "https"
        } else {
            "http"
        };
        base.set_scheme(scheme)
            .map_err(|_| RpcError::internal("Invalid LiveKit URL"))?;
        base.set_path(&format!("/twirp/livekit.RoomService/{method}"));
        let body = match identity {
            Some(id) => json!({"room":Self::room(call),"identity":id}),
            None => json!({"room":Self::room(call)}),
        };
        let response = reqwest::Client::new()
            .post(base)
            .timeout(std::time::Duration::from_secs(5))
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .map_err(|_| RpcError::internal("LiveKit cleanup unavailable"))?;
        if response.status().is_success() {
            tx.commit().await.map_err(db)?;
            return Ok(());
        }
        // Only Twirp not_found is an idempotent success, not every HTTP 404.
        let body: Value = response.json().await.unwrap_or(Value::Null);
        tx.commit().await.map_err(db)?;
        if body.get("code").and_then(Value::as_str) == Some("not_found") {
            Ok(())
        } else {
            Err(RpcError::internal("LiveKit cleanup rejected"))
        }
    }
}
