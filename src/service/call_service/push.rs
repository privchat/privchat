//! Call-only durable delivery. Never route through the chat PushPayload/message fallback.
use super::*;
use async_trait::async_trait;
use std::sync::Arc;
use uuid::Uuid;

mod provider;
pub use provider::CallHttpPushProvider;

#[derive(Clone)]
pub struct CallPushNotification {
    pub platform: CallPushPlatform,
    pub token: String,
    pub ordinary_apns_token: Option<String>,
    pub incoming: bool,
    pub delivery_id: Uuid,
    pub data: BTreeMap<String, String>,
    pub expires_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPushResult {
    Accepted,
    Retry,
    InvalidToken,
    Rejected,
    NoControlToken,
}

#[async_trait]
pub trait CallPushProvider: Send + Sync {
    fn supports(&self, platform: CallPushPlatform) -> bool;
    async fn send(&self, notification: &CallPushNotification) -> CallPushResult;
}

fn platform_name(platform: CallPushPlatform) -> &'static str {
    match platform {
        CallPushPlatform::ApnsVoip => "apns_voip",
        CallPushPlatform::Fcm => "fcm",
    }
}
fn device_uuid(device: &str) -> RpcResult<Uuid> {
    Uuid::parse_str(device).map_err(|_| RpcError::validation("Registered device UUID required"))
}

impl CallService {
    pub async fn register_push_device(
        &self,
        actor: u64,
        device: &str,
        authenticated_session_version: i64,
        mut request: CallPushRegisterRequest,
    ) -> RpcResult<CallPushRegistrationResponse> {
        self.enabled()?;
        if !self.config.push.enabled {
            return Err(RpcError::from_code(
                privchat_protocol::ErrorCode::ServiceUnavailable,
                "Call push disabled".into(),
            ));
        }
        let device = device_uuid(device)?;
        let valid = match request.platform {
            CallPushPlatform::ApnsVoip => {
                (32..=512).contains(&request.token.len())
                    && request.token.len() % 2 == 0
                    && request.token.bytes().all(|c| c.is_ascii_hexdigit())
            }
            CallPushPlatform::Fcm => {
                (1..=2048).contains(&request.token.len())
                    && request.token.bytes().all(|c| c.is_ascii_graphic())
            }
        };
        if !valid {
            return Err(RpcError::validation("Invalid call push token"));
        }
        if request.platform == CallPushPlatform::ApnsVoip {
            request.token.make_ascii_lowercase();
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        let row = sqlx::query("SELECT session_version,device_type FROM privchat_devices WHERE device_id=$1 AND user_id=$2 AND session_state=0 FOR SHARE")
            .bind(device).bind(actor as i64).fetch_optional(&mut *tx).await.map_err(db)?
            .ok_or_else(|| RpcError::forbidden("Active owned device required"))?;
        let kind: String = row.get("device_type");
        if row.get::<i64, _>("session_version") != authenticated_session_version {
            return Err(RpcError::unauthorized(
                "Device session changed; authenticate again",
            ));
        }
        if !matches!(
            (request.platform, kind.to_ascii_lowercase().as_str()),
            (CallPushPlatform::ApnsVoip, "ios") | (CallPushPlatform::Fcm, "android")
        ) {
            return Err(RpcError::validation(
                "Call push platform does not match authenticated device",
            ));
        }
        // A physical token re-bound after an account switch must stop targeting the old account.
        let token_hash = hex::encode(Sha256::digest(request.token.as_bytes()));
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("call-push-token:{token_hash}"))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("DELETE FROM privchat_call_push_devices WHERE platform=$1 AND token=$2 AND (user_id<>$3 OR device_id<>$4)")
            .bind(platform_name(request.platform)).bind(&request.token).bind(actor as i64).bind(device).execute(&mut *tx).await.map_err(db)?;
        sqlx::query("INSERT INTO privchat_call_push_devices(user_id,device_id,platform,token,session_version,updated_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT(user_id,device_id) DO UPDATE SET platform=EXCLUDED.platform,token=EXCLUDED.token,session_version=EXCLUDED.session_version,updated_at=EXCLUDED.updated_at")
            .bind(actor as i64).bind(device).bind(platform_name(request.platform)).bind(request.token).bind(row.get::<i64,_>("session_version")).bind(now()).execute(&mut *tx).await.map_err(db)?;
        let calls = sqlx::query("SELECT document FROM privchat_calls WHERE state<>'ended' AND document->'call'->'participants' @> $1::jsonb ORDER BY call_id FOR UPDATE")
            .bind(json!([{"user_id":actor}])).fetch_all(&mut *tx).await.map_err(db)?;
        for row in calls {
            let stored: StoredCall = serde_json::from_value(row.get("document"))
                .map_err(|_| RpcError::internal("Invalid call state"))?;
            reconcile(&mut tx, &stored).await?;
            sqlx::query("UPDATE privchat_call_push_outbox SET status='pending',attempts=0,next_attempt_at=0,last_error=NULL WHERE call_id=$1 AND user_id=$2 AND device_id=$3 AND kind='incoming' AND status IN ('failed','unavailable') AND expires_at>$4")
                .bind(stored.call.call_id as i64).bind(actor as i64).bind(device).bind(now()).execute(&mut *tx).await.map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        Ok(CallPushRegistrationResponse { registered: true })
    }

    pub async fn unregister_push_device(
        &self,
        actor: u64,
        device: &str,
    ) -> RpcResult<CallPushRegistrationResponse> {
        let device = device_uuid(device)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        sqlx::query("DELETE FROM privchat_call_push_devices WHERE user_id=$1 AND device_id=$2")
            .bind(actor as i64)
            .bind(device)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        sqlx::query("UPDATE privchat_call_push_outbox SET status='cancelled',last_error='unregistered' WHERE user_id=$1 AND device_id=$2 AND status='pending'")
            .bind(actor as i64).bind(device).execute(&mut *tx).await.map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(CallPushRegistrationResponse { registered: false })
    }
}

fn eligible(call: &CallSnapshot, user: u64, timestamp: i64) -> bool {
    call.state != CallState::Ended
        && call.expires_at_ms > timestamp
        && user != call.initiator_id
        && call
            .participants
            .iter()
            .any(|p| p.user_id == user && p.state == CallParticipantState::Invited)
}

pub(super) async fn reconcile(
    tx: &mut Transaction<'_, Postgres>,
    stored: &StoredCall,
) -> RpcResult<()> {
    let call = &stored.call;
    let timestamp = now();
    for p in &call.participants {
        if eligible(call, p.user_id, timestamp) {
            sqlx::query("INSERT INTO privchat_call_push_outbox(call_id,user_id,device_id,kind,call_version,expires_at) SELECT $1,r.user_id,r.device_id,'incoming',$2,$3 FROM privchat_call_push_devices r JOIN privchat_devices d ON d.device_id=r.device_id AND d.user_id=r.user_id AND d.session_version=r.session_version WHERE r.user_id=$4 AND d.session_state=0 ON CONFLICT DO NOTHING")
                .bind(call.call_id as i64).bind(call.version as i64).bind(call.expires_at_ms).bind(p.user_id as i64).execute(&mut **tx).await.map_err(db)?;
        } else {
            sqlx::query("UPDATE privchat_call_push_outbox SET status='cancelled',last_error='call_state_changed' WHERE call_id=$1 AND user_id=$2 AND kind='incoming' AND status='pending'")
                .bind(call.call_id as i64).bind(p.user_id as i64).execute(&mut **tx).await.map_err(db)?;
            let winner =
                if call.state != CallState::Ended && p.state == CallParticipantState::Joined {
                    stored
                        .claims
                        .get(&p.user_id)
                        .and_then(|c| Uuid::parse_str(&c.device).ok())
                } else {
                    None
                };
            // Include uncommitted/unknown provider outcomes after a crash; cancel data is never VoIP.
            sqlx::query("INSERT INTO privchat_call_push_outbox(call_id,user_id,device_id,kind,call_version,expires_at) SELECT call_id,user_id,device_id,'cancel',$3,$4 FROM privchat_call_push_outbox WHERE call_id=$1 AND user_id=$2 AND kind='incoming' AND ($5::uuid IS NULL OR device_id<>$5) ON CONFLICT(call_id,user_id,device_id,kind) DO UPDATE SET call_version=EXCLUDED.call_version")
                .bind(call.call_id as i64).bind(p.user_id as i64).bind(call.version as i64).bind(timestamp+30000).bind(winner).execute(&mut **tx).await.map_err(db)?;
        }
    }
    Ok(())
}

pub struct CallPushWorker {
    service: CallService,
    provider: Arc<dyn CallPushProvider>,
}
impl CallPushWorker {
    pub fn from_config(
        service: CallService,
        config: &crate::config::PushConfig,
    ) -> anyhow::Result<Self> {
        Ok(Self::new(
            service,
            Arc::new(CallHttpPushProvider::from_config(config)?),
        ))
    }
    pub fn new(service: CallService, provider: Arc<dyn CallPushProvider>) -> Self {
        Self { service, provider }
    }
    pub fn supports(&self, platform: CallPushPlatform) -> bool {
        self.provider.supports(platform)
    }

    pub async fn dispatch_one(&self) -> anyhow::Result<bool> {
        let mut tx = self.service.pool.begin().await?;
        // Match call-command lock order: call before outbox. This avoids deadlocking an answer.
        let row=sqlx::query("SELECT q.id,c.document FROM privchat_call_push_outbox q JOIN privchat_calls c ON c.call_id=q.call_id WHERE q.status='pending' AND q.next_attempt_at<=$1 ORDER BY q.id FOR UPDATE OF c SKIP LOCKED LIMIT 1")
            .bind(now()).fetch_optional(&mut *tx).await?;
        let Some(row) = row else { return Ok(false) };
        let id: i64 = row.get("id");
        let stored: StoredCall = serde_json::from_value(row.get("document"))?;
        let q = sqlx::query("SELECT * FROM privchat_call_push_outbox WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await?;
        if q.get::<String, _>("status") != "pending" {
            tx.commit().await?;
            return Ok(true);
        }
        let user = q.get::<i64, _>("user_id") as u64;
        let device: Uuid = q.get("device_id");
        let incoming = q.get::<String, _>("kind") == "incoming";
        let expiry: i64 = q.get("expires_at");
        let (_, members) = CallService::members(&mut tx, stored.call.channel_id).await?;
        let mut authorized = members.contains_key(&user);
        if incoming {
            match CallService::not_blocked(&mut tx, stored.call.initiator_id, user).await {
                Ok(()) => (),
                Err(e) if e.code == privchat_protocol::ErrorCode::PermissionDenied => {
                    authorized = false
                }
                Err(e) => return Err(e.into()),
            }
        }
        let winner = stored.call.state != CallState::Ended
            && stored
                .claims
                .get(&user)
                .is_some_and(|c| c.device == device.to_string());
        let stale = expiry <= now()
            || !authorized
            || (incoming && !eligible(&stored.call, user, now()))
            || (!incoming && winner);
        if stale {
            sqlx::query("UPDATE privchat_call_push_outbox SET status=$2,last_error='stale_or_unauthorized' WHERE id=$1").bind(id).bind(if expiry<=now(){"expired"}else{"cancelled"}).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(true);
        }
        let registration=sqlx::query("SELECT r.platform,r.token,r.session_version,(SELECT u.push_token FROM privchat_user_devices u WHERE u.user_id=r.user_id AND u.device_id=r.device_id::text AND u.vendor='apns' AND u.apns_armed=true) AS ordinary_token FROM privchat_call_push_devices r JOIN privchat_devices d ON d.device_id=r.device_id AND d.user_id=r.user_id AND d.session_version=r.session_version JOIN privchat_users u ON u.user_id=r.user_id WHERE r.user_id=$1 AND r.device_id=$2 AND d.session_state=0 AND u.status=0")
            .bind(user as i64).bind(device).fetch_optional(&mut *tx).await?;
        let Some(r) = registration else {
            sqlx::query("UPDATE privchat_call_push_outbox SET status='cancelled',last_error='device_unavailable' WHERE id=$1").bind(id).execute(&mut *tx).await?;
            tx.commit().await?;
            return Ok(true);
        };
        let platform = if r.get::<String, _>("platform") == "apns_voip" {
            CallPushPlatform::ApnsVoip
        } else {
            CallPushPlatform::Fcm
        };
        let mut data = BTreeMap::new();
        for (k, v) in [
            ("schema_version", "1".into()),
            (
                "type",
                if incoming {
                    "call.incoming"
                } else {
                    "call.cancel"
                }
                .into(),
            ),
            (
                "call_uuid",
                Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("privchat:call:{}", stored.call.call_id).as_bytes(),
                )
                .to_string(),
            ),
            ("call_id", stored.call.call_id.to_string()),
            ("channel_id", stored.call.channel_id.to_string()),
            ("initiator_id", stored.call.initiator_id.to_string()),
            ("recipient_user_id", user.to_string()),
            (
                "media_kind",
                if stored.call.media_kind == CallMediaKind::Video {
                    "video"
                } else {
                    "audio"
                }
                .into(),
            ),
            (
                "transport",
                if stored.call.transport == CallTransport::Webrtc {
                    "webrtc"
                } else {
                    "livekit"
                }
                .into(),
            ),
            ("version", stored.call.version.to_string()),
            ("expires_at_ms", expiry.to_string()),
        ] {
            data.insert(k.to_owned(), v);
        }
        let n = CallPushNotification {
            platform,
            token: r.get("token"),
            ordinary_apns_token: r.get("ordinary_token"),
            incoming,
            delivery_id: Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("privchat:call-push:{id}").as_bytes(),
            ),
            data,
            expires_at_ms: expiry,
        };
        let attempts = q.get::<i32, _>("attempts") + 1;
        // Bound the entire operation, including OAuth refresh; never hold an answer behind a 15s provider timeout.
        let result = tokio::time::timeout(
            std::time::Duration::from_millis((expiry - now()).clamp(1, 3000) as u64),
            self.provider.send(&n),
        )
        .await
        .unwrap_or(CallPushResult::Retry);
        let (status, error) = match result {
            CallPushResult::Accepted => ("sent", None),
            CallPushResult::NoControlToken => ("unavailable", Some("no_control_token")),
            CallPushResult::InvalidToken => ("failed", Some("invalid_token")),
            CallPushResult::Rejected => ("failed", Some("provider_rejected")),
            CallPushResult::Retry if attempts >= self.service.config.push.max_attempts as i32 => {
                ("failed", Some("retry_exhausted"))
            }
            CallPushResult::Retry => ("pending", Some("transient_provider_error")),
        };
        sqlx::query("UPDATE privchat_call_push_outbox SET status=$2,attempts=$3,next_attempt_at=$4,last_error=$5 WHERE id=$1")
            .bind(id).bind(status).bind(attempts).bind(now()+1000*(1i64<<attempts.min(3))).bind(error).execute(&mut *tx).await?;
        // Registration takes token/device rows before active call rows. Release the call/outbox
        // locks before invalidation, and compare the exact token/version so rotation cannot be erased.
        tx.commit().await?;
        if result == CallPushResult::InvalidToken && incoming {
            sqlx::query("DELETE FROM privchat_call_push_devices WHERE user_id=$1 AND device_id=$2 AND token=$3 AND session_version=$4")
                .bind(user as i64).bind(device).bind(&n.token).bind(r.get::<i64,_>("session_version")).execute(&self.service.pool).await?;
        }
        Ok(true)
    }
}
