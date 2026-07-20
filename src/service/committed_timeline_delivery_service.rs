// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev

//! Reliable post-commit timeline dispatch.
//!
//! `DispatchOutboxStore` owns the PostgreSQL lease/fencing state machine.
//! `CommittedTimelineDeliveryService` owns mapping and online/offline handoff.

use crate::error::{Result, ServerError};
use crate::infra::MessageRouter;
use crate::repository::PgMessageRepository;
use crate::service::OfflineQueueService;
use futures::stream::{self, StreamExt};
use privchat_protocol::{
    CanonicalTimelineEvent, ContentMessageType, FlatBufferMessage, MessageSetting,
    PushMessageRequest, CANONICAL_TIMELINE_EVENT_SCHEMA_V1, CANONICAL_TIMELINE_PUSH_TOPIC_V1,
};
use sqlx::{FromRow, PgPool, Row};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

const MAX_ATTEMPTS: i32 = 8;
const LEASE_DURATION_MS: i64 = 30_000;
const DEFAULT_CHUNK_SIZE: i64 = 100;
const PER_BATCH_CONCURRENCY: usize = 50;
const MAX_IMMEDIATE_EVENTS: usize = 8;
const WORKER_IDLE_DELAY: Duration = Duration::from_millis(100);
const PARENT_RECONCILE_INTERVAL: Duration = Duration::from_secs(1);
const RETENTION_SWEEP_INTERVAL: Duration = Duration::from_secs(60);
const RETENTION_BATCH_SIZE: i64 = 1_000;
const DISPATCHED_RETENTION_MS: i64 = 24 * 60 * 60 * 1_000;
const DEAD_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1_000;

#[derive(Debug, Clone, FromRow)]
pub struct ClaimedDispatchRecipient {
    pub event_id: i64,
    pub user_id: i64,
    pub attempts: i32,
    pub lease_token: i64,
    pub pts: i64,
    pub server_msg_id: i64,
    pub local_message_id: Option<i64>,
    pub channel_id: i64,
    /// Wire channel type from commit_log (Direct=1, Group=2, Room=3).
    pub channel_type: i16,
    pub message_type: String,
    pub content: serde_json::Value,
    pub server_timestamp: i64,
    pub sender_id: i64,
    pub event_schema_version: Option<i16>,
    pub canonical_event: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchRecipientState {
    OnlineSent = 1,
    OfflineQueued = 2,
    Dead = 3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientDeliveryOutcome {
    OnlineSent,
    OfflineQueued,
    RetryScheduled {
        error: String,
    },
    Dead {
        error: String,
    },
    Fenced,
    /// Dispatch finished, but the fenced database writeback failed. The row is
    /// still owned until its lease expires, so it must not be reported as a
    /// scheduled retry.
    CompletionFailed {
        error: String,
    },
}

#[derive(Debug, Clone)]
pub struct DispatchOutcome {
    pub event_id: u64,
    pub user_id: u64,
    pub outcome: RecipientDeliveryOutcome,
}

#[derive(Debug, Clone)]
struct DispatchCompletion {
    claim: ClaimedDispatchRecipient,
    terminal_state: Option<DispatchRecipientState>,
    error_message: Option<String>,
}

impl DispatchCompletion {
    fn new(
        claim: ClaimedDispatchRecipient,
        terminal_state: Option<DispatchRecipientState>,
        error_message: Option<String>,
    ) -> Self {
        Self {
            claim,
            terminal_state,
            error_message,
        }
    }
}

enum DispatchResolution {
    Completion(DispatchCompletion),
    OfflineCandidate {
        claim: ClaimedDispatchRecipient,
        push: PushMessageRequest,
    },
}

type OfflineDispatchBatches = BTreeMap<i64, (PushMessageRequest, Vec<ClaimedDispatchRecipient>)>;

fn partition_dispatch_resolutions(
    resolutions: Vec<DispatchResolution>,
) -> (Vec<DispatchCompletion>, OfflineDispatchBatches) {
    let mut completions = Vec::with_capacity(resolutions.len());
    let mut offline_by_event = BTreeMap::new();
    for resolution in resolutions {
        match resolution {
            DispatchResolution::Completion(completion) => completions.push(completion),
            DispatchResolution::OfflineCandidate { claim, push } => {
                let entry = offline_by_event
                    .entry(claim.event_id)
                    .or_insert_with(|| (push, Vec::new()));
                entry.1.push(claim);
            }
        }
    }
    (completions, offline_by_event)
}

#[derive(Clone)]
pub struct DispatchOutboxStore {
    pool: Arc<PgPool>,
}

impl DispatchOutboxStore {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    pub async fn claim_pending(
        &self,
        owner: &str,
        limit: i64,
    ) -> Result<Vec<ClaimedDispatchRecipient>> {
        self.claim(owner, limit, None).await
    }

    pub async fn claim_event(
        &self,
        owner: &str,
        event_id: u64,
    ) -> Result<Vec<ClaimedDispatchRecipient>> {
        self.claim(owner, DEFAULT_CHUNK_SIZE, Some(event_id as i64))
            .await
    }

    async fn claim(
        &self,
        owner: &str,
        limit: i64,
        event_id: Option<i64>,
    ) -> Result<Vec<ClaimedDispatchRecipient>> {
        let now = chrono::Utc::now().timestamp_millis();
        let deadline = now + LEASE_DURATION_MS;
        let lease_token = (rand::random::<u64>() & i64::MAX as u64) as i64;
        let rows = sqlx::query_as::<_, ClaimedDispatchRecipient>(
            r#"
            WITH candidates AS (
                SELECT r.event_id, r.user_id
                FROM privchat_message_dispatch_recipient r
                JOIN privchat_message_dispatch_outbox o ON o.event_id = r.event_id
                WHERE r.state = 0
                  AND r.attempts < $1
                  AND o.status = 0
                  AND r.next_attempt_at <= $2
                  AND (r.lease_until IS NULL OR r.lease_until < $2)
                  AND ($6::BIGINT IS NULL OR r.event_id = $6)
                ORDER BY r.event_id, r.user_id
                FOR UPDATE OF r SKIP LOCKED
                LIMIT $3
            ), claimed AS (
                UPDATE privchat_message_dispatch_recipient r
                SET lease_owner = $4,
                    lease_until = $5,
                    lease_token = $7,
                    attempts = r.attempts + 1
                FROM candidates c
                WHERE r.event_id = c.event_id AND r.user_id = c.user_id
                RETURNING r.event_id, r.user_id, r.attempts, r.lease_token
            )
            SELECT claimed.event_id, claimed.user_id, claimed.attempts,
                   claimed.lease_token, c.pts, c.server_msg_id,
                   c.local_message_id, c.channel_id, c.channel_type,
                   c.message_type, c.content, c.server_timestamp, c.sender_id,
                   c.event_schema_version, c.canonical_event
            FROM claimed
            JOIN privchat_commit_log c ON c.id = claimed.event_id
            ORDER BY claimed.event_id, claimed.user_id
            "#,
        )
        .bind(MAX_ATTEMPTS)
        .bind(now)
        .bind(limit.clamp(1, 1_000))
        .bind(owner)
        .bind(deadline)
        .bind(event_id)
        .bind(lease_token)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("claim dispatch recipients failed: {e}")))?;
        Ok(rows)
    }

    pub async fn complete(
        &self,
        claim: &ClaimedDispatchRecipient,
        terminal_state: Option<DispatchRecipientState>,
        error_message: Option<&str>,
    ) -> Result<RecipientDeliveryOutcome> {
        let completion = DispatchCompletion::new(
            claim.clone(),
            terminal_state,
            error_message.map(str::to_string),
        );
        let mut outcomes = self.complete_many(&[completion]).await?;
        self.reconcile_events(&[claim.event_id]).await?;
        Ok(outcomes.pop().expect("single completion has one outcome"))
    }

    async fn complete_many(
        &self,
        completions: &[DispatchCompletion],
    ) -> Result<Vec<RecipientDeliveryOutcome>> {
        if completions.is_empty() {
            return Ok(Vec::new());
        }
        let now = chrono::Utc::now().timestamp_millis();
        let mut event_ids = Vec::with_capacity(completions.len());
        let mut user_ids = Vec::with_capacity(completions.len());
        let mut lease_tokens = Vec::with_capacity(completions.len());
        let mut states = Vec::with_capacity(completions.len());
        let mut errors = Vec::with_capacity(completions.len());
        let mut retry_times = Vec::with_capacity(completions.len());
        let mut planned_outcomes = Vec::with_capacity(completions.len());

        for completion in completions {
            let (state, retry_at, outcome) = completion_values(
                &completion.claim,
                completion.terminal_state,
                completion.error_message.as_deref(),
                now,
            );
            event_ids.push(completion.claim.event_id);
            user_ids.push(completion.claim.user_id);
            lease_tokens.push(completion.claim.lease_token);
            states.push(state);
            errors.push(completion.error_message.clone());
            retry_times.push(retry_at);
            planned_outcomes.push(outcome);
        }

        let rows = sqlx::query(
            r#"
            WITH input AS (
                SELECT *
                FROM UNNEST(
                    $1::BIGINT[], $2::BIGINT[], $3::BIGINT[], $4::SMALLINT[],
                    $5::TEXT[], $6::BIGINT[]
                ) WITH ORDINALITY AS i(
                    event_id, user_id, lease_token, state, last_error,
                    next_attempt_at, ordinal
                )
            ), updated AS (
                UPDATE privchat_message_dispatch_recipient r
                SET state = i.state,
                    last_error = i.last_error,
                    next_attempt_at = i.next_attempt_at,
                    lease_owner = NULL,
                    lease_until = NULL
                FROM input i
                WHERE r.event_id = i.event_id AND r.user_id = i.user_id
                  AND r.state = 0 AND r.lease_token = i.lease_token
                RETURNING r.event_id, r.user_id
            )
            SELECT i.ordinal, (u.event_id IS NOT NULL) AS updated
            FROM input i
            LEFT JOIN updated u
              ON u.event_id = i.event_id AND u.user_id = i.user_id
            ORDER BY i.ordinal
            "#,
        )
        .bind(&event_ids)
        .bind(&user_ids)
        .bind(&lease_tokens)
        .bind(&states)
        .bind(&errors)
        .bind(&retry_times)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("complete dispatch recipients failed: {e}")))?;

        Ok(rows
            .into_iter()
            .zip(planned_outcomes)
            .map(|(row, outcome)| {
                if row.get::<bool, _>("updated") {
                    outcome
                } else {
                    RecipientDeliveryOutcome::Fenced
                }
            })
            .collect())
    }

    pub async fn reconcile_terminal_parents(&self, limit: i64) -> Result<u64> {
        let now = chrono::Utc::now().timestamp_millis();
        let result = sqlx::query(
            r#"
            WITH candidates AS (
                SELECT o.event_id
                FROM privchat_message_dispatch_outbox o
                WHERE o.status = 0
                  AND NOT EXISTS (
                      SELECT 1 FROM privchat_message_dispatch_recipient r
                      WHERE r.event_id = o.event_id AND r.state = 0
                  )
                ORDER BY o.event_id
                FOR UPDATE SKIP LOCKED
                LIMIT $1
            )
            UPDATE privchat_message_dispatch_outbox o
            SET status = CASE WHEN EXISTS (
                    SELECT 1 FROM privchat_message_dispatch_recipient r
                    WHERE r.event_id = o.event_id AND r.state = 3
                ) THEN 2 ELSE 1 END,
                dispatched_at = CASE WHEN NOT EXISTS (
                    SELECT 1 FROM privchat_message_dispatch_recipient r
                    WHERE r.event_id = o.event_id AND r.state = 3
                ) THEN COALESCE(o.dispatched_at, $2) ELSE o.dispatched_at END
            FROM candidates c
            WHERE o.event_id = c.event_id
            "#,
        )
        .bind(limit.clamp(1, 1_000))
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("reconcile dispatch parents failed: {e}")))?;
        Ok(result.rows_affected())
    }

    async fn reconcile_events(&self, event_ids: &[i64]) -> Result<u64> {
        if event_ids.is_empty() {
            return Ok(0);
        }
        let now = chrono::Utc::now().timestamp_millis();
        let result = sqlx::query(
            r#"
            UPDATE privchat_message_dispatch_outbox o
            SET status = CASE WHEN EXISTS (
                    SELECT 1 FROM privchat_message_dispatch_recipient r
                    WHERE r.event_id = o.event_id AND r.state = 3
                ) THEN 2 ELSE 1 END,
                dispatched_at = CASE WHEN NOT EXISTS (
                    SELECT 1 FROM privchat_message_dispatch_recipient r
                    WHERE r.event_id = o.event_id AND r.state = 3
                ) THEN COALESCE(o.dispatched_at, $2) ELSE o.dispatched_at END
            WHERE o.event_id = ANY($1::BIGINT[])
              AND o.status = 0
              AND NOT EXISTS (
                  SELECT 1 FROM privchat_message_dispatch_recipient r
                  WHERE r.event_id = o.event_id AND r.state = 0
              )
            "#,
        )
        .bind(event_ids)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("reconcile dispatch events failed: {e}")))?;
        Ok(result.rows_affected())
    }

    pub async fn cleanup_retained(&self, now_ms: i64, limit: i64) -> Result<u64> {
        let result = sqlx::query(
            r#"
            WITH expired AS (
                SELECT event_id
                FROM privchat_message_dispatch_outbox
                WHERE (status = 1 AND created_at < $1)
                   OR (status = 2 AND created_at < $2)
                ORDER BY created_at, event_id
                FOR UPDATE SKIP LOCKED
                LIMIT $3
            )
            DELETE FROM privchat_message_dispatch_outbox o
            USING expired e
            WHERE o.event_id = e.event_id
            "#,
        )
        .bind(now_ms - DISPATCHED_RETENTION_MS)
        .bind(now_ms - DEAD_RETENTION_MS)
        .bind(limit.clamp(1, 10_000))
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("cleanup dispatch outbox failed: {e}")))?;
        Ok(result.rows_affected())
    }
}

#[derive(Clone)]
pub struct CommittedTimelineDeliveryService {
    store: DispatchOutboxStore,
    message_router: Arc<MessageRouter>,
    message_repository: Arc<PgMessageRepository>,
    offline_queue: Arc<OfflineQueueService>,
    global_fanout: Arc<Semaphore>,
    immediate_events: Arc<Semaphore>,
    worker_id: String,
}

impl CommittedTimelineDeliveryService {
    pub fn new(
        pool: Arc<PgPool>,
        message_router: Arc<MessageRouter>,
        message_repository: Arc<PgMessageRepository>,
        offline_queue: Arc<OfflineQueueService>,
        global_fanout: Arc<Semaphore>,
        worker_id: impl Into<String>,
    ) -> Self {
        Self {
            store: DispatchOutboxStore::new(pool),
            message_router,
            message_repository,
            offline_queue,
            global_fanout,
            immediate_events: Arc::new(Semaphore::new(MAX_IMMEDIATE_EVENTS)),
            worker_id: worker_id.into(),
        }
    }

    pub async fn dispatch_event(&self, event_id: u64) -> Result<Vec<DispatchOutcome>> {
        let Ok(_event_permit) = self.immediate_events.try_acquire() else {
            // The row is durable and remains pending. Never create an unbounded
            // queue of per-event tasks in memory; the outbox worker will claim it.
            return Ok(Vec::new());
        };
        let claims = self.store.claim_event(&self.worker_id, event_id).await?;
        self.dispatch_claims(claims).await
    }

    pub async fn process_once(&self) -> Result<Vec<DispatchOutcome>> {
        let claims = self
            .store
            .claim_pending(&self.worker_id, DEFAULT_CHUNK_SIZE)
            .await?;
        self.dispatch_claims(claims).await
    }

    pub async fn start(&self) -> Result<()> {
        info!(worker_id = %self.worker_id, "CommittedTimelineDeliveryService started");
        let mut last_retention_sweep = tokio::time::Instant::now() - RETENTION_SWEEP_INTERVAL;
        let mut last_parent_reconcile = tokio::time::Instant::now() - PARENT_RECONCILE_INTERVAL;
        loop {
            if last_parent_reconcile.elapsed() >= PARENT_RECONCILE_INTERVAL {
                if let Err(error) = self
                    .store
                    .reconcile_terminal_parents(DEFAULT_CHUNK_SIZE)
                    .await
                {
                    warn!(%error, "dispatch outbox parent reconciliation failed");
                }
                last_parent_reconcile = tokio::time::Instant::now();
            }
            if last_retention_sweep.elapsed() >= RETENTION_SWEEP_INTERVAL {
                match self
                    .store
                    .cleanup_retained(chrono::Utc::now().timestamp_millis(), RETENTION_BATCH_SIZE)
                    .await
                {
                    Ok(removed) if removed > 0 => {
                        info!(removed, "dispatch outbox retention sweep completed")
                    }
                    Ok(_) => {}
                    Err(error) => warn!(%error, "dispatch outbox retention sweep failed"),
                }
                last_retention_sweep = tokio::time::Instant::now();
            }
            match self.process_once().await {
                Ok(outcomes) if outcomes.is_empty() => tokio::time::sleep(WORKER_IDLE_DELAY).await,
                Ok(_) => {}
                Err(e) => {
                    error!(error = %e, "dispatch outbox worker iteration failed");
                    tokio::time::sleep(WORKER_IDLE_DELAY).await;
                }
            }
        }
    }

    async fn dispatch_claims(
        &self,
        claims: Vec<ClaimedDispatchRecipient>,
    ) -> Result<Vec<DispatchOutcome>> {
        let service = self.clone();
        let resolutions: Vec<DispatchResolution> = stream::iter(claims)
            .map(move |claim| {
                let service = service.clone();
                async move { service.resolve_online_claim(claim).await }
            })
            .buffer_unordered(PER_BATCH_CONCURRENCY)
            .collect()
            .await;

        let (mut completions, offline_by_event) = partition_dispatch_resolutions(resolutions);

        // Every recipient of one event receives the same wire payload. Persist
        // all offline copies with one Redis pipeline instead of one round trip
        // per user, while retaining one fenced PostgreSQL state row per user.
        let service = self.clone();
        let offline_completions: Vec<Vec<DispatchCompletion>> =
            stream::iter(offline_by_event.into_values())
                .map(move |(push, claims)| {
                    let service = service.clone();
                    async move { service.resolve_offline_batch(push, claims).await }
                })
                .buffer_unordered(PER_BATCH_CONCURRENCY)
                .collect()
                .await;
        completions.extend(offline_completions.into_iter().flatten());

        let delivery_outcomes = self.store.complete_many(&completions).await?;
        let mut event_ids: Vec<i64> = completions
            .iter()
            .map(|completion| completion.claim.event_id)
            .collect();
        event_ids.sort_unstable();
        event_ids.dedup();
        let outcomes = completions
            .into_iter()
            .zip(delivery_outcomes)
            .map(|(completion, outcome)| {
                if matches!(outcome, RecipientDeliveryOutcome::Dead { .. }) {
                    warn!(
                        event_id = completion.claim.event_id,
                        user_id = completion.claim.user_id,
                        error = completion
                            .error_message
                            .as_deref()
                            .unwrap_or("dispatch exhausted"),
                        "dispatch recipient DEAD"
                    );
                }
                DispatchOutcome {
                    event_id: completion.claim.event_id as u64,
                    user_id: completion.claim.user_id as u64,
                    outcome,
                }
            })
            .collect();

        // Recipient completions are independent fenced writes. Aggregate parent
        // state once per dispatch batch instead of making every recipient race
        // for the same parent tuple (100 recipients used to mean 100 contenders).
        self.store.reconcile_events(&event_ids).await?;
        Ok(outcomes)
    }

    async fn resolve_online_claim(&self, claim: ClaimedDispatchRecipient) -> DispatchResolution {
        let _permit = match self.global_fanout.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                return DispatchResolution::Completion(DispatchCompletion::new(
                    claim,
                    None,
                    Some("global fanout semaphore closed".to_string()),
                ));
            }
        };

        let push = match push_from_claim(&claim) {
            Ok(push) => push,
            Err(e) => {
                return DispatchResolution::Completion(DispatchCompletion::new(
                    claim,
                    None,
                    Some(e.to_string()),
                ));
            }
        };
        match self
            .message_router
            .route_message_to_user(&(claim.user_id as u64), push.clone())
            .await
        {
            Ok(route) if route.success_count > 0 => {
                if let Some(ack_session_id) = route
                    .delivery_report
                    .as_ref()
                    .and_then(|report| report.acknowledged_sessions.first())
                    .copied()
                {
                    if let Err(error) = self
                        .record_and_push_delivered_receipt(&claim, ack_session_id)
                        .await
                    {
                        return DispatchResolution::Completion(DispatchCompletion::new(
                            claim,
                            None,
                            Some(error.to_string()),
                        ));
                    }
                }
                DispatchResolution::Completion(DispatchCompletion::new(
                    claim,
                    Some(DispatchRecipientState::OnlineSent),
                    None,
                ))
            }
            Ok(route) if route.failed_count > 0 => {
                DispatchResolution::Completion(DispatchCompletion::new(
                    claim,
                    None,
                    Some("one or more local/cross-node routes failed".to_string()),
                ))
            }
            Ok(_) => DispatchResolution::OfflineCandidate { claim, push },
            Err(e) => DispatchResolution::Completion(DispatchCompletion::new(
                claim,
                None,
                Some(e.to_string()),
            )),
        }
    }

    async fn resolve_offline_batch(
        &self,
        push: PushMessageRequest,
        claims: Vec<ClaimedDispatchRecipient>,
    ) -> Vec<DispatchCompletion> {
        let _permit = match self.global_fanout.acquire().await {
            Ok(permit) => permit,
            Err(_) => {
                return claims
                    .into_iter()
                    .map(|claim| {
                        DispatchCompletion::new(
                            claim,
                            None,
                            Some("global fanout semaphore closed".to_string()),
                        )
                    })
                    .collect();
            }
        };
        let user_ids: Vec<u64> = claims.iter().map(|claim| claim.user_id as u64).collect();
        let result = self.offline_queue.add_batch_users(&user_ids, &push).await;
        claims
            .into_iter()
            .map(|claim| match &result {
                Ok(()) => DispatchCompletion::new(
                    claim,
                    Some(DispatchRecipientState::OfflineQueued),
                    None,
                ),
                Err(error) => DispatchCompletion::new(claim, None, Some(error.to_string())),
            })
            .collect()
    }

    async fn record_and_push_delivered_receipt(
        &self,
        claim: &ClaimedDispatchRecipient,
        ack_session_id: msgtrans::SessionId,
    ) -> Result<()> {
        if claim.channel_type != 1 {
            return Ok(());
        }
        let delivered_at = chrono::Utc::now().timestamp_millis() as u64;
        let receipt = self
            .message_repository
            .record_direct_delivery_receipt_first_ack(
                claim.server_msg_id as u64,
                claim.user_id as u64,
                ack_session_id.as_u64(),
                delivered_at,
            )
            .await
            .map_err(|error| {
                ServerError::Database(format!(
                    "persist delivered receipt failed for message {}: {error}",
                    claim.server_msg_id
                ))
            })?;
        let Some(receipt) = receipt else {
            return Ok(());
        };

        let notification = privchat_protocol::notification::MessageDeliveryReceiptNotification::new(
            receipt.channel_id,
            1,
            receipt.server_message_id,
            receipt.recipient_user_id,
            receipt.delivered_at,
        );
        let payload = serde_json::to_vec(&notification).map_err(|error| {
            ServerError::Protocol(format!(
                "encode delivered receipt for message {} failed: {error}",
                receipt.server_message_id
            ))
        })?;
        let push = PushMessageRequest {
            setting: Default::default(),
            msg_key: String::new(),
            server_message_id: receipt.server_message_id,
            message_seq: claim.pts as u32,
            local_message_id: 0,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: chrono::Utc::now().timestamp() as u32,
            channel_id: receipt.channel_id,
            channel_type: 1,
            message_type: ContentMessageType::System.as_u32(),
            expire: 0,
            topic: String::new(),
            from_uid: receipt.recipient_user_id,
            payload,
            deleted: false,
        };
        if let Err(error) = self
            .message_router
            .route_message_to_user(&receipt.sender_id, push)
            .await
        {
            warn!(
                sender_id = receipt.sender_id,
                server_message_id = receipt.server_message_id,
                %error,
                "push delivered receipt notification failed"
            );
        }
        Ok(())
    }
}

fn retry_backoff_ms(attempts: i32) -> i64 {
    let exponent = attempts.saturating_sub(1).clamp(0, 30) as u32;
    let base = (1_000i64.saturating_mul(1i64 << exponent)).min(30_000);
    base + fastrand::i64(0..=250)
}

fn completion_values(
    claim: &ClaimedDispatchRecipient,
    terminal_state: Option<DispatchRecipientState>,
    error_message: Option<&str>,
    now: i64,
) -> (i16, i64, RecipientDeliveryOutcome) {
    match terminal_state {
        Some(DispatchRecipientState::OnlineSent) => (
            DispatchRecipientState::OnlineSent as i16,
            now,
            RecipientDeliveryOutcome::OnlineSent,
        ),
        Some(DispatchRecipientState::OfflineQueued) => (
            DispatchRecipientState::OfflineQueued as i16,
            now,
            RecipientDeliveryOutcome::OfflineQueued,
        ),
        Some(DispatchRecipientState::Dead) => (
            DispatchRecipientState::Dead as i16,
            now,
            RecipientDeliveryOutcome::Dead {
                error: error_message.unwrap_or("dispatch exhausted").to_string(),
            },
        ),
        None if claim.attempts >= MAX_ATTEMPTS => (
            DispatchRecipientState::Dead as i16,
            now,
            RecipientDeliveryOutcome::Dead {
                error: error_message.unwrap_or("dispatch exhausted").to_string(),
            },
        ),
        None => (
            0,
            now + retry_backoff_ms(claim.attempts),
            RecipientDeliveryOutcome::RetryScheduled {
                error: error_message.unwrap_or("dispatch failed").to_string(),
            },
        ),
    }
}

fn push_from_claim(claim: &ClaimedDispatchRecipient) -> Result<PushMessageRequest> {
    let event = if claim.event_schema_version == Some(CANONICAL_TIMELINE_EVENT_SCHEMA_V1 as i16) {
        claim
            .canonical_event
            .as_deref()
            .map(CanonicalTimelineEvent::decode_fb)
            .transpose()
            .map_err(|e| ServerError::Protocol(format!("decode canonical event failed: {e}")))?
    } else {
        None
    }
    .or_else(|| {
        CanonicalTimelineEvent::from_legacy(
            &claim.message_type,
            &claim.content,
            claim.server_msg_id as u64,
            claim.sender_id as u64,
            claim.server_timestamp,
        )
        .ok()
        .flatten()
    })
    .ok_or_else(|| ServerError::Protocol("commit has no mappable timeline event".to_string()))?;

    let is_new_message = matches!(event, CanonicalTimelineEvent::NewMessage(_));
    let (server_message_id, local_message_id, message_type, topic, from_uid, payload) = match &event
    {
        CanonicalTimelineEvent::NewMessage(message) => (
            claim.server_msg_id as u64,
            claim.local_message_id.unwrap_or(0) as u64,
            message.message_type.as_u32(),
            String::new(),
            claim.sender_id as u64,
            // 滚动兼容（生产乱码事故根修）：NewMessage 推送 payload 用 **legacy JSON envelope**，
            // 三代客户端都能解——旧 APK（JSON-first，解不了 FB 会 lossy 成「控制符+长度字节+正文」
            // 写本地库）、新 TS（JSON 优先）、新 Rust SDK（FB 失败落 JSON）。FlatBuffers 推送升级
            // 必须等客户端能力协商（CODEX-9 P1/D2 additive migration）就位后再开，禁止再裸切。
            serde_json::to_vec(&message.payload.to_legacy()).map_err(|e| {
                ServerError::Protocol(format!("encode message payload failed: {e}"))
            })?,
        ),
        CanonicalTimelineEvent::Revoke(revoke) => (
            claim.event_id as u64,
            0,
            ContentMessageType::System.as_u32(),
            CANONICAL_TIMELINE_PUSH_TOPIC_V1.to_string(),
            revoke.revoked_by,
            event
                .encode_fb()
                .map_err(|e| ServerError::Protocol(format!("encode revoke event failed: {e}")))?,
        ),
        CanonicalTimelineEvent::ReactionChange(reaction) => (
            claim.event_id as u64,
            0,
            ContentMessageType::System.as_u32(),
            CANONICAL_TIMELINE_PUSH_TOPIC_V1.to_string(),
            reaction.actor_id,
            event
                .encode_fb()
                .map_err(|e| ServerError::Protocol(format!("encode reaction event failed: {e}")))?,
        ),
    };
    Ok(PushMessageRequest {
        setting: MessageSetting {
            need_receipt: claim.channel_type == 1 && is_new_message,
            signal: 0,
        },
        msg_key: if is_new_message {
            format!("msg_{}", claim.server_msg_id)
        } else {
            format!("event_{}", claim.event_id)
        },
        server_message_id,
        message_seq: claim.pts as u32,
        local_message_id,
        stream_no: String::new(),
        stream_seq: 0,
        stream_flag: 0,
        timestamp: (claim.server_timestamp / 1_000).max(0) as u32,
        channel_id: claim.channel_id as u64,
        channel_type: claim.channel_type as u8,
        message_type,
        expire: 0,
        topic,
        from_uid,
        payload,
        deleted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use privchat_protocol::{
        ContentMessageType, MessagePayloadEnvelope, NewMessageEvent, RevokeEvent,
    };
    use sqlx::postgres::PgPoolOptions;

    const EVENT_ID: i64 = 987_675_001;
    const CHANNEL_ID: i64 = 987_675_002;
    const SENDER_ID: i64 = 987_675_003;
    const RECIPIENT_ID: i64 = 987_675_004;

    fn claim_fixture() -> ClaimedDispatchRecipient {
        let event = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: ContentMessageType::Text,
            payload: MessagePayloadEnvelope {
                content: "typed dispatch".to_string(),
                ..Default::default()
            },
        });
        ClaimedDispatchRecipient {
            event_id: 1,
            user_id: 2,
            attempts: 1,
            lease_token: 3,
            pts: 4,
            server_msg_id: 5,
            local_message_id: Some(6),
            channel_id: 7,
            channel_type: 1,
            message_type: "text".to_string(),
            content: serde_json::json!({"content": "typed dispatch"}),
            server_timestamp: 8_000,
            sender_id: 9,
            event_schema_version: Some(CANONICAL_TIMELINE_EVENT_SCHEMA_V1 as i16),
            canonical_event: Some(event.encode_fb().unwrap()),
        }
    }

    #[test]
    fn offline_candidates_for_one_event_share_one_pipeline_batch() {
        let first = claim_fixture();
        let mut second = first.clone();
        second.user_id += 1;
        let push = push_from_claim(&first).expect("claim maps to push");
        let (completions, batches) = partition_dispatch_resolutions(vec![
            DispatchResolution::OfflineCandidate {
                claim: first,
                push: push.clone(),
            },
            DispatchResolution::OfflineCandidate {
                claim: second,
                push,
            },
        ]);

        assert!(completions.is_empty());
        assert_eq!(batches.len(), 1);
        let (_push, claims) = batches.into_values().next().expect("one event batch");
        assert_eq!(claims.len(), 2);
        assert_ne!(claims[0].user_id, claims[1].user_id);
    }

    // 契约变更（生产乱码事故根修）：NewMessage 推送 payload = legacy JSON envelope（滚动兼容，
    // 旧 APK/新 TS/新 Rust 三代都能解），不再是 FlatBuffers（原断言随契约更新）。
    #[test]
    fn mapper_preserves_typed_payload_and_id_roles() {
        let push = push_from_claim(&claim_fixture()).unwrap();
        // 1) 必须是 legacy JSON envelope（旧 APK JSON-first 可解）
        let legacy: privchat_protocol::message::LocalMessagePayloadEnvelope =
            serde_json::from_slice(&push.payload).expect("legacy JSON envelope payload");
        assert_eq!(legacy.content, "typed dispatch");
        // 2) 绝不能再是裸 FlatBuffers（那会让旧 APK lossy 成「控制符+长度字节+正文」）
        assert!(
            privchat_protocol::decode_message::<MessagePayloadEnvelope>(&push.payload).is_err(),
            "NewMessage push payload must be legacy JSON until client capability negotiation exists"
        );
        // 3) 无 C0 控制字节（\t\n\r 除外）——三代客户端的纯文本/二进制守卫都不会误判
        assert!(!push
            .payload
            .iter()
            .any(|&b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r'));
        assert_eq!(push.server_message_id, 5);
        assert_eq!(push.local_message_id, 6);
        assert_eq!(push.channel_id, 7);
        assert_eq!(push.message_seq, 4);
        assert!(push.setting.need_receipt);
    }

    // 滚动兼容保真：reply/mentions 经 legacy JSON envelope 不丢。
    #[test]
    fn new_message_push_payload_keeps_reply_and_mentions() {
        let event = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: ContentMessageType::Text,
            payload: MessagePayloadEnvelope {
                content: "with refs".to_string(),
                reply_to_message_id: Some(4242),
                mentioned_user_ids: vec![11, 22],
                ..Default::default()
            },
        });
        let mut claim = claim_fixture();
        claim.canonical_event = Some(event.encode_fb().unwrap());
        let push = push_from_claim(&claim).unwrap();
        let legacy: privchat_protocol::message::LocalMessagePayloadEnvelope =
            serde_json::from_slice(&push.payload).expect("legacy JSON envelope payload");
        assert_eq!(legacy.content, "with refs");
        assert_eq!(legacy.reply_to_message_id.as_deref(), Some("4242"));
        assert_eq!(legacy.mentioned_user_ids, Some(vec![11, 22]));
    }

    #[test]
    fn mapper_carries_mutations_as_canonical_flatbuffers() {
        let mut claim = claim_fixture();
        let event = CanonicalTimelineEvent::Revoke(RevokeEvent {
            target_server_message_id: 99,
            revoked_by: 88,
            revoked_at: 77,
        });
        claim.canonical_event = Some(event.encode_fb().unwrap());
        claim.message_type = "message.revoke".to_string();
        let push = push_from_claim(&claim).unwrap();
        assert_eq!(push.topic, CANONICAL_TIMELINE_PUSH_TOPIC_V1);
        assert_eq!(push.server_message_id, claim.event_id as u64);
        assert_eq!(push.from_uid, 88);
        assert!(!push.setting.need_receipt);
        assert_eq!(
            CanonicalTimelineEvent::decode_fb(&push.payload).unwrap(),
            event
        );
    }

    #[test]
    fn retry_backoff_is_bounded() {
        for attempts in 1..=MAX_ATTEMPTS {
            let delay = retry_backoff_ms(attempts);
            assert!((1_000..=30_250).contains(&delay));
        }
    }

    async fn open_store() -> Option<DispatchOutboxStore> {
        let url = std::env::var("PRIVCHAT_TEST_DATABASE_URL")
            .or_else(|_| std::env::var("DATABASE_URL"))
            .ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(&url)
            .await
            .ok()?;
        Some(DispatchOutboxStore::new(Arc::new(pool)))
    }

    async fn cleanup(store: &DispatchOutboxStore) {
        let _ = sqlx::query("DELETE FROM privchat_message_dispatch_outbox WHERE event_id = $1")
            .bind(EVENT_ID)
            .execute(store.pool.as_ref())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_commit_log WHERE id = $1")
            .bind(EVENT_ID)
            .execute(store.pool.as_ref())
            .await;
        let _ = sqlx::query("DELETE FROM privchat_channels WHERE channel_id = $1")
            .bind(CHANNEL_ID)
            .execute(store.pool.as_ref())
            .await;
    }

    async fn seed_outbox(store: &DispatchOutboxStore) {
        cleanup(store).await;
        for user_id in [SENDER_ID, RECIPIENT_ID] {
            sqlx::query(
                r#"
                INSERT INTO privchat_users (user_id, username, display_name, qr_key)
                VALUES ($1, $2, $2, $3)
                ON CONFLICT (user_id) DO NOTHING
                "#,
            )
            .bind(user_id)
            .bind(format!("p5{}", user_id % 1_000_000))
            .bind(format!("qp5{}", user_id % 1_000_000))
            .execute(store.pool.as_ref())
            .await
            .unwrap();
        }
        sqlx::query(
            r#"
            INSERT INTO privchat_channels
                (channel_id, channel_type, direct_user1_id, direct_user2_id)
            VALUES ($1, 0, $2, $3)
            "#,
        )
        .bind(CHANNEL_ID)
        .bind(SENDER_ID)
        .bind(RECIPIENT_ID)
        .execute(store.pool.as_ref())
        .await
        .unwrap();

        let event = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: ContentMessageType::Text,
            payload: MessagePayloadEnvelope {
                content: "outbox-state-machine".to_string(),
                ..Default::default()
            },
        });
        let now = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            r#"
            INSERT INTO privchat_commit_log
                (id, pts, server_msg_id, local_message_id, channel_id, channel_type,
                 message_type, content, server_timestamp, sender_id, created_at,
                 event_schema_version, canonical_event)
            VALUES ($1, 1, $1, $1, $2, 1, 'text', $3, $4, $5, $4, $6, $7)
            "#,
        )
        .bind(EVENT_ID)
        .bind(CHANNEL_ID)
        .bind(serde_json::json!({"content": "outbox-state-machine"}))
        .bind(now)
        .bind(SENDER_ID)
        .bind(CANONICAL_TIMELINE_EVENT_SCHEMA_V1 as i16)
        .bind(event.encode_fb().unwrap())
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO privchat_message_dispatch_outbox
                (event_id, channel_id, channel_type, pts, sender_id,
                 event_kind, membership_version, status, created_at)
            VALUES ($1, $2, 0, 1, $3, 1, 0, 0, $4)
            "#,
        )
        .bind(EVENT_ID)
        .bind(CHANNEL_ID)
        .bind(SENDER_ID)
        .bind(now)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        for user_id in [SENDER_ID, RECIPIENT_ID] {
            sqlx::query(
                r#"
                INSERT INTO privchat_message_dispatch_recipient
                    (event_id, user_id, state, attempts, next_attempt_at)
                VALUES ($1, $2, 0, 0, 0)
                "#,
            )
            .bind(EVENT_ID)
            .bind(user_id)
            .execute(store.pool.as_ref())
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn outbox_claim_is_fenced_retriable_and_aggregated() {
        let _fixture_guard = crate::database_fixture_lock().lock().await;
        let Some(store) = open_store().await else {
            eprintln!("skip outbox state test: DATABASE_URL not configured");
            return;
        };
        seed_outbox(&store).await;

        let claims = store
            .claim_event("worker-a", EVENT_ID as u64)
            .await
            .unwrap();
        assert_eq!(claims.len(), 2);
        assert!(store
            .claim_event("worker-b", EVENT_ID as u64)
            .await
            .unwrap()
            .is_empty());

        let mut stale = claims[0].clone();
        stale.lease_token = stale.lease_token.wrapping_add(1);
        assert_eq!(
            store
                .complete(&stale, Some(DispatchRecipientState::OnlineSent), None)
                .await
                .unwrap(),
            RecipientDeliveryOutcome::Fenced
        );

        let retry = store
            .complete(&claims[0], None, Some("redis timeout"))
            .await
            .unwrap();
        assert!(matches!(
            retry,
            RecipientDeliveryOutcome::RetryScheduled { .. }
        ));
        let lease_cleared: bool = sqlx::query_scalar(
            r#"
            SELECT lease_owner IS NULL AND lease_until IS NULL
            FROM privchat_message_dispatch_recipient
            WHERE event_id = $1 AND user_id = $2
            "#,
        )
        .bind(EVENT_ID)
        .bind(claims[0].user_id)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert!(lease_cleared);

        store
            .complete(&claims[1], Some(DispatchRecipientState::OnlineSent), None)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE privchat_message_dispatch_recipient SET next_attempt_at = 0 WHERE event_id = $1 AND state = 0",
        )
        .bind(EVENT_ID)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        let retried = store
            .claim_event("worker-b", EVENT_ID as u64)
            .await
            .unwrap();
        assert_eq!(retried.len(), 1);
        assert_eq!(retried[0].attempts, 2);
        assert_ne!(retried[0].lease_token, claims[0].lease_token);
        store
            .complete(
                &retried[0],
                Some(DispatchRecipientState::OfflineQueued),
                None,
            )
            .await
            .unwrap();

        let parent: (i16, Option<i64>) = sqlx::query_as(
            "SELECT status, dispatched_at FROM privchat_message_dispatch_outbox WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(parent.0, 1);
        assert!(parent.1.is_some());
        cleanup(&store).await;
    }

    #[tokio::test]
    async fn expired_lease_is_reclaimed_after_worker_crash() {
        let _fixture_guard = crate::database_fixture_lock().lock().await;
        let Some(store) = open_store().await else {
            eprintln!("skip outbox crash-recovery test: DATABASE_URL not configured");
            return;
        };
        seed_outbox(&store).await;

        let abandoned = store
            .claim_event("crashed-worker", EVENT_ID as u64)
            .await
            .unwrap();
        assert_eq!(abandoned.len(), 2);
        assert!(store
            .claim_event("replacement-worker", EVENT_ID as u64)
            .await
            .unwrap()
            .is_empty());

        // Simulate the crashed worker's 30-second lease expiring without a
        // completion writeback. The replacement must receive fresh fencing
        // tokens, while the crashed worker can no longer mutate the rows.
        sqlx::query(
            "UPDATE privchat_message_dispatch_recipient SET lease_until = 0 WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        let recovered = store
            .claim_event("replacement-worker", EVENT_ID as u64)
            .await
            .unwrap();
        assert_eq!(recovered.len(), 2);
        assert!(recovered
            .iter()
            .all(|claim| claim.attempts == 2 && claim.lease_token != abandoned[0].lease_token));
        assert_eq!(
            store
                .complete(
                    &abandoned[0],
                    Some(DispatchRecipientState::OnlineSent),
                    None,
                )
                .await
                .unwrap(),
            RecipientDeliveryOutcome::Fenced
        );
        for claim in &recovered {
            store
                .complete(claim, Some(DispatchRecipientState::OfflineQueued), None)
                .await
                .unwrap();
        }
        let status: i16 = sqlx::query_scalar(
            "SELECT status FROM privchat_message_dispatch_outbox WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(status, 1);
        cleanup(&store).await;
    }

    #[tokio::test]
    async fn persistent_offline_queue_failure_reaches_dead_after_bounded_attempts() {
        let _fixture_guard = crate::database_fixture_lock().lock().await;
        let Some(store) = open_store().await else {
            eprintln!("skip outbox persistent-failure test: DATABASE_URL not configured");
            return;
        };
        seed_outbox(&store).await;

        for attempt in 1..=MAX_ATTEMPTS {
            let claims = store
                .claim_event("redis-failure-worker", EVENT_ID as u64)
                .await
                .unwrap();
            assert_eq!(
                claims.len(),
                2,
                "attempt {attempt} must claim both recipients"
            );
            for claim in &claims {
                let outcome = store
                    .complete(claim, None, Some("injected Redis timeout"))
                    .await
                    .unwrap();
                if attempt < MAX_ATTEMPTS {
                    assert!(matches!(
                        outcome,
                        RecipientDeliveryOutcome::RetryScheduled { .. }
                    ));
                } else {
                    assert!(matches!(outcome, RecipientDeliveryOutcome::Dead { .. }));
                }
            }
            if attempt < MAX_ATTEMPTS {
                sqlx::query(
                    "UPDATE privchat_message_dispatch_recipient SET next_attempt_at = 0 WHERE event_id = $1 AND state = 0",
                )
                .bind(EVENT_ID)
                .execute(store.pool.as_ref())
                .await
                .unwrap();
            }
        }

        let parent_status: i16 = sqlx::query_scalar(
            "SELECT status FROM privchat_message_dispatch_outbox WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        let dead_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM privchat_message_dispatch_recipient WHERE event_id = $1 AND state = 3",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(parent_status, 2);
        assert_eq!(dead_count, 2);
        cleanup(&store).await;
    }

    #[tokio::test]
    async fn concurrent_recipient_completions_finalize_parent() {
        let _fixture_guard = crate::database_fixture_lock().lock().await;
        let Some(store) = open_store().await else {
            eprintln!("skip concurrent parent aggregation test: DATABASE_URL not configured");
            return;
        };
        seed_outbox(&store).await;
        let claims = store
            .claim_event("parallel-worker", EVENT_ID as u64)
            .await
            .unwrap();
        assert_eq!(claims.len(), 2);

        let left_store = store.clone();
        let left = claims[0].clone();
        let right_store = store.clone();
        let right = claims[1].clone();
        let (left_result, right_result) = tokio::join!(
            async move {
                left_store
                    .complete(&left, Some(DispatchRecipientState::OnlineSent), None)
                    .await
            },
            async move {
                right_store
                    .complete(&right, Some(DispatchRecipientState::OnlineSent), None)
                    .await
            }
        );
        left_result.unwrap();
        right_result.unwrap();

        let status: i16 = sqlx::query_scalar(
            "SELECT status FROM privchat_message_dispatch_outbox WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(status, 1);
        cleanup(&store).await;
    }

    #[tokio::test]
    async fn retention_uses_distinct_dispatched_and_dead_windows() {
        let _fixture_guard = crate::database_fixture_lock().lock().await;
        let Some(store) = open_store().await else {
            eprintln!("skip outbox retention test: DATABASE_URL not configured");
            return;
        };
        let now = chrono::Utc::now().timestamp_millis();

        seed_outbox(&store).await;
        sqlx::query(
            "UPDATE privchat_message_dispatch_outbox SET status = 1, created_at = $2 WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .bind(now - DISPATCHED_RETENTION_MS - 1)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(store.cleanup_retained(now, 10).await.unwrap(), 1);
        let recipient_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM privchat_message_dispatch_recipient WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(recipient_count, 0, "recipient rows must cascade-delete");

        seed_outbox(&store).await;
        sqlx::query(
            "UPDATE privchat_message_dispatch_outbox SET status = 2, created_at = $2 WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .bind(now - DISPATCHED_RETENTION_MS - 1)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(
            store.cleanup_retained(now, 10).await.unwrap(),
            0,
            "DEAD rows must survive the shorter DISPATCHED window"
        );
        sqlx::query(
            "UPDATE privchat_message_dispatch_outbox SET created_at = $2 WHERE event_id = $1",
        )
        .bind(EVENT_ID)
        .bind(now - DEAD_RETENTION_MS - 1)
        .execute(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(store.cleanup_retained(now, 10).await.unwrap(), 1);
        cleanup(&store).await;
    }
}
