//! Bounded, private signalling journal. Never copy SDP/ICE into logs or chat events.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use sha1::Sha1;

impl CallService {
    pub(super) fn peer_config(&self, call: &CallSnapshot, actor: u64) -> RpcResult<CallPeerConfig> {
        if !self.config.peer.enabled {
            return Err(RpcError::forbidden("Peer transport unavailable"));
        }
        let mut credential_expires_at_ms = 0;
        let mut ice_servers = Vec::new();
        if !self.config.peer.stun_urls.is_empty() {
            ice_servers.push(CallIceServer {
                urls: self.config.peer.stun_urls.clone(),
                username: None,
                credential: None,
            });
        }
        if self.config.peer.allow_turn {
            let secret = self
                .turn_secret
                .as_ref()
                .ok_or_else(|| RpcError::internal("TURN unavailable"))?;
            let expires = now() / 1000 + i64::from(self.config.peer.credential_ttl_seconds);
            let username = format!("{expires}:{}:{actor}", call.call_id);
            let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes())
                .map_err(|_| RpcError::internal("TURN credential failure"))?;
            mac.update(username.as_bytes());
            let credential = STANDARD.encode(mac.finalize().into_bytes());
            ice_servers.push(CallIceServer {
                urls: self.config.peer.turn_urls.clone(),
                username: Some(username),
                credential: Some(credential),
            });
            credential_expires_at_ms = expires * 1000;
        }
        Ok(CallPeerConfig {
            self_user_id: actor,
            peer_user_id: call
                .participants
                .iter()
                .find(|p| p.user_id != actor)
                .ok_or_else(|| RpcError::internal("Peer missing"))?
                .user_id,
            offerer: actor == call.initiator_id,
            ice_servers,
            credential_expires_at_ms,
        })
    }

    pub(super) async fn clear_peer(tx: &mut Transaction<'_, Postgres>, call: u64) -> RpcResult<()> {
        for table in [
            "privchat_call_peer_events",
            "privchat_call_peer_commands",
            "privchat_call_peer_rates",
        ] {
            sqlx::query(&format!("DELETE FROM {table} WHERE call_id=$1"))
                .bind(call as i64)
                .execute(&mut **tx)
                .await
                .map_err(db)?;
        }
        Ok(())
    }

    pub(super) async fn sweep_peer(&self) -> RpcResult<()> {
        for table in ["privchat_call_peer_events", "privchat_call_peer_commands"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE expires_at <= $1"))
                .bind(now())
                .execute(&self.pool)
                .await
                .map_err(db)?;
        }
        sqlx::query("DELETE FROM privchat_call_peer_rates WHERE bucket < $1")
            .bind(now() / 10000 - 2)
            .execute(&self.pool)
            .await
            .map_err(db)?;
        Ok(())
    }

    async fn authorize_peer(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        actor: u64,
        device: &str,
        call: u64,
    ) -> RpcResult<StoredCall> {
        self.enabled()?;
        let mut stored = Self::load(tx, call).await?;
        let (_, members) = Self::members(tx, stored.call.channel_id).await?;
        if !members.contains_key(&actor) {
            return Err(RpcError::forbidden("Not a channel member"));
        }
        self.expire(tx, &mut stored).await?;
        if stored.call.state == CallState::Ended
            || stored.call.transport != CallTransport::Webrtc
            || stored.channel_type != 0
        {
            return Err(RpcError::forbidden(
                "Peer signalling unavailable for this call",
            ));
        }
        if !stored
            .claims
            .get(&actor)
            .is_some_and(|c| c.device == device && c.lease_until > now())
            || !stored
                .call
                .participants
                .iter()
                .any(|p| p.user_id == actor && p.state == CallParticipantState::Joined)
        {
            return Err(RpcError::forbidden(
                "Only the joined winning device may signal",
            ));
        }
        for peer in &stored.call.participants {
            if peer.user_id != actor {
                Self::not_blocked(tx, actor, peer.user_id).await?;
            }
        }
        Ok(stored)
    }

    async fn peer_rate(
        tx: &mut Transaction<'_, Postgres>,
        call: u64,
        actor: u64,
        op: &str,
        limit: i32,
    ) -> RpcResult<()> {
        let count: i32 = sqlx::query_scalar("INSERT INTO privchat_call_peer_rates(call_id,user_id,operation,bucket,count) VALUES($1,$2,$3,$4,1) ON CONFLICT(call_id,user_id,operation) DO UPDATE SET bucket=EXCLUDED.bucket,count=CASE WHEN privchat_call_peer_rates.bucket=EXCLUDED.bucket THEN privchat_call_peer_rates.count+1 ELSE 1 END RETURNING count")
            .bind(call as i64).bind(actor as i64).bind(op).bind(now()/10000).fetch_one(&mut **tx).await.map_err(db)?;
        if count > limit {
            return Err(RpcError::validation(
                "Peer signalling rate limit; retry later",
            ));
        }
        Ok(())
    }

    fn peer_event(row: &sqlx::postgres::PgRow) -> RpcResult<CallPeerEvent> {
        Ok(CallPeerEvent {
            seq: row.get::<i64, _>("seq") as u64,
            revision: row.get::<i64, _>("revision") as u32,
            kind: match row.get::<String, _>("kind").as_str() {
                "offer" => CallPeerSignalKind::Offer,
                "answer" => CallPeerSignalKind::Answer,
                "ice" => CallPeerSignalKind::Ice,
                _ => return Err(RpcError::internal("Invalid peer event")),
            },
            payload: row.get("payload"),
            from_user_id: row.get::<i64, _>("from_user_id") as u64,
        })
    }

    pub async fn peer_signal(
        &self,
        actor: u64,
        device: &str,
        req: CallPeerSignalRequest,
    ) -> RpcResult<CallPeerEventsResponse> {
        self.enabled()?;
        if device.is_empty()
            || device.len() > 256
            || req.command_id.trim().is_empty()
            || req.command_id.len() > 128
            || req.revision == 0
        {
            return Err(RpcError::validation("Invalid peer command or revision"));
        }
        let limit = if req.kind == CallPeerSignalKind::Ice {
            8192
        } else {
            65536
        };
        if req.payload.trim().is_empty() || req.payload.len() > limit {
            return Err(RpcError::validation(
                "Peer payload empty or exceeds size limit",
            ));
        }
        if req.kind == CallPeerSignalKind::Ice {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Candidate {
                candidate: String,
                sdp_mid: Option<String>,
                sdp_m_line_index: u16,
            }
            let c: Candidate = serde_json::from_str(&req.payload)
                .map_err(|_| RpcError::validation("Invalid ICE candidate envelope"))?;
            if c.candidate.len() > 8192
                || c.sdp_mid.as_ref().is_some_and(|s| s.len() > 256)
                || c.sdp_m_line_index > 255
            {
                return Err(RpcError::validation("Invalid ICE candidate fields"));
            }
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("call-command:{actor}:{device}:{}", req.command_id))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let mut stored = self
            .authorize_peer(&mut tx, actor, device, req.call_id)
            .await?;
        Self::peer_rate(&mut tx, req.call_id, actor, "signal", 60).await?;
        let fingerprint = hex::encode(Sha256::digest(
            serde_json::to_vec(&req).map_err(|_| RpcError::validation("Invalid peer command"))?,
        ));
        let existing = sqlx::query("SELECT call_id,fingerprint,seq,expires_at FROM privchat_call_peer_commands WHERE user_id=$1 AND device_id=$2 AND command_id=$3")
            .bind(actor as i64).bind(device).bind(&req.command_id).fetch_optional(&mut *tx).await.map_err(db)?;
        if let Some(row) = existing {
            if row.get::<String, _>("fingerprint") != fingerprint
                || row.get::<i64, _>("call_id") != req.call_id as i64
            {
                return Err(RpcError::validation(
                    "command_id reused with a different peer request",
                ));
            }
            if row.get::<i64, _>("expires_at") <= now() || req.revision != stored.peer_revision {
                return Err(RpcError::validation(
                    "Peer replay expired or revision superseded",
                ));
            }
            let event = sqlx::query("SELECT * FROM privchat_call_peer_events WHERE call_id=$1 AND seq=$2 AND expires_at>$3")
                .bind(req.call_id as i64).bind(row.get::<i64,_>("seq")).bind(now()).fetch_optional(&mut *tx).await.map_err(db)?
                .ok_or_else(||RpcError::validation("Peer replay expired"))?;
            let response = CallPeerEventsResponse {
                events: vec![Self::peer_event(&event)?],
                next_seq: row.get::<i64, _>("seq") as u64,
            };
            tx.commit().await.map_err(db)?;
            return Ok(response);
        }
        // Prevent a peer command from reusing a control-plane command's identity.
        let used: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM privchat_call_commands WHERE user_id=$1 AND device_id=$2 AND command_id=$3)")
            .bind(actor as i64).bind(device).bind(&req.command_id).fetch_one(&mut *tx).await.map_err(db)?;
        if used {
            return Err(RpcError::validation(
                "command_id already used by call control",
            ));
        }
        match req.kind {
            CallPeerSignalKind::Offer => {
                if actor != stored.call.initiator_id
                    || stored.peer_revision.checked_add(1) != Some(req.revision)
                {
                    return Err(RpcError::forbidden(
                        "Only initiator may offer the next revision",
                    ));
                }
                stored.peer_revision = req.revision;
                stored.peer_answered = false;
            }
            CallPeerSignalKind::Answer => {
                if actor == stored.call.initiator_id
                    || req.revision != stored.peer_revision
                    || stored.peer_answered
                {
                    return Err(RpcError::forbidden(
                        "Answer must match the unanswered live revision",
                    ));
                }
                stored.peer_answered = true;
            }
            CallPeerSignalKind::Ice => {
                if req.revision != stored.peer_revision {
                    return Err(RpcError::validation("ICE must match live revision"));
                }
            }
        }
        sqlx::query("DELETE FROM privchat_call_peer_events WHERE call_id=$1 AND expires_at<=$2")
            .bind(req.call_id as i64)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM privchat_call_peer_events WHERE call_id=$1")
                .bind(req.call_id as i64)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?;
        if count >= 512 {
            return Err(RpcError::validation(
                "Peer journal full; retry after expiry",
            ));
        }
        stored.peer_seq += 1;
        let expires = now() + i64::from(self.config.peer.journal_ttl_seconds) * 1000;
        let kind = match req.kind {
            CallPeerSignalKind::Offer => "offer",
            CallPeerSignalKind::Answer => "answer",
            CallPeerSignalKind::Ice => "ice",
        };
        sqlx::query("INSERT INTO privchat_call_peer_events(call_id,seq,revision,kind,payload,from_user_id,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(req.call_id as i64).bind(stored.peer_seq as i64).bind(i64::from(req.revision)).bind(kind).bind(&req.payload).bind(actor as i64).bind(expires).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO privchat_call_peer_commands(call_id,user_id,device_id,command_id,fingerprint,seq,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(req.call_id as i64).bind(actor as i64).bind(device).bind(&req.command_id).bind(fingerprint).bind(stored.peer_seq as i64).bind(expires).execute(&mut *tx).await.map_err(db)?;
        Self::save(&mut tx, &stored, false).await?;
        let response = CallPeerEventsResponse {
            events: vec![CallPeerEvent {
                seq: stored.peer_seq,
                revision: req.revision,
                kind: req.kind,
                payload: req.payload,
                from_user_id: actor,
            }],
            next_seq: stored.peer_seq,
        };
        tx.commit().await.map_err(db)?;
        Ok(response)
    }

    pub async fn peer_events(
        &self,
        actor: u64,
        device: &str,
        req: CallPeerEventsRequest,
    ) -> RpcResult<CallPeerEventsResponse> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let stored = self
            .authorize_peer(&mut tx, actor, device, req.call_id)
            .await?;
        Self::peer_rate(&mut tx, req.call_id, actor, "events", 40).await?;
        if req.after_seq > stored.peer_seq {
            return Err(RpcError::validation("Peer cursor exceeds current sequence"));
        }
        let rows = sqlx::query("SELECT * FROM privchat_call_peer_events WHERE call_id=$1 AND expires_at>$2 ORDER BY seq LIMIT 512")
            .bind(req.call_id as i64).bind(now()).fetch_all(&mut *tx).await.map_err(db)?;
        let first = rows
            .first()
            .map(|r| r.get::<i64, _>("seq") as u64)
            .unwrap_or(stored.peer_seq + 1);
        if req.after_seq != 0 && req.after_seq + 1 < first {
            return Err(RpcError::validation(
                "Peer journal expired; restart negotiation and read after_seq=0",
            ));
        }
        if req.after_seq == 0
            && stored.peer_revision > 0
            && !rows.iter().any(|r| {
                r.get::<i64, _>("revision") == i64::from(stored.peer_revision)
                    && r.get::<String, _>("kind") == "offer"
            })
        {
            return Err(RpcError::validation(
                "Peer offer expired; initiator must restart negotiation",
            ));
        }
        let events = rows
            .iter()
            .filter(|r| {
                r.get::<i64, _>("seq") as u64 > req.after_seq
                    && r.get::<i64, _>("from_user_id") != actor as i64
                    && r.get::<i64, _>("revision") == i64::from(stored.peer_revision)
            })
            .map(Self::peer_event)
            .collect::<RpcResult<Vec<_>>>()?;
        tx.commit().await.map_err(db)?;
        Ok(CallPeerEventsResponse {
            events,
            next_seq: stored.peer_seq,
        })
    }
}
