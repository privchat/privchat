// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Room 持续授权（ROOM_CHANNEL_SPEC §4.8）。
//!
//! 入场票据只证明「签发时有权进入」。本服务补上入场之后的三件事：
//!
//! 1. **必须持票的 Room**：创建时声明 `ticket_required` 的 Room，在未配置 `[room_ticket]`
//!    时订阅一律拒绝，不走「已认证即放行」的兼容模式。
//! 2. **到期**：订阅记住票据 exp；到期后停止投递并清除订阅。
//! 3. **撤权**：按 Room + UID（+ 可选 deviceId）撤销；立即清除现有订阅，签发时间早于
//!    撤权时间的旧票据不得再入场。撤权持久化，重启后仍然有效。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use msgtrans::SessionId;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::error::{Result, ServerError};
use crate::infra::{ConnectionManager, SubscribeManager};

/// 一次带票据的 Room 订阅授权。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomGrant {
    pub user_id: u64,
    pub device_id: String,
    /// 票据 exp（Unix 秒）。
    pub expires_at: u64,
    pub issued_at: u64,
}

pub struct RoomAccessService {
    pool: Arc<PgPool>,
    subscribe_manager: Arc<SubscribeManager>,
    mutation: Mutex<()>,
    /// session → (channel → grant)。只记录经票据校验进入的订阅。
    grants: DashMap<SessionId, HashMap<u64, RoomGrant>>,
    /// channel → ticket_required 的读缓存（策略只在创建时写入，不会改）。
    policies: DashMap<u64, bool>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 票据签发时间（秒）是否早于撤权时间（毫秒）——早于即视为被撤销。
/// 同一秒内签发的票据也视为被撤销：秒级 iat 无法证明它晚于撤权。
pub fn issued_before_revocation(ticket_iat_secs: u64, revoked_at_ms: i64) -> bool {
    i64::try_from(ticket_iat_secs)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1000))
        .is_none_or(|issued| issued <= revoked_at_ms)
}

impl RoomAccessService {
    pub fn new(
        pool: Arc<PgPool>,
        subscribe_manager: Arc<SubscribeManager>,
        _connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            pool,
            subscribe_manager,
            mutation: Mutex::new(()),
            grants: DashMap::new(),
            policies: DashMap::new(),
        }
    }

    /// 创建 Room 时登记策略。
    pub async fn set_ticket_required(&self, channel_id: u64, required: bool) -> Result<()> {
        sqlx::query(
            "INSERT INTO privchat_room_policies (channel_id, ticket_required, created_at) VALUES ($1, $2, $3)
             ON CONFLICT (channel_id) DO NOTHING",
        )
        .bind(channel_id as i64)
        .bind(required)
        .bind(now_ms())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("room policy insert: {e}")))?;
        self.policies.remove(&channel_id);
        Ok(())
    }

    /// 是否必须持票。查询失败按「必须」处理（fail-closed）。
    pub async fn ticket_required(&self, channel_id: u64) -> bool {
        if let Some(v) = self.policies.get(&channel_id) {
            return *v;
        }
        let row: std::result::Result<Option<(bool,)>, _> = sqlx::query_as(
            "SELECT ticket_required FROM privchat_room_policies WHERE channel_id = $1",
        )
        .bind(channel_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await;
        match row {
            Ok(v) => {
                let required = v.map(|r| r.0).unwrap_or(false);
                // An absent policy can be registered by another server later.
                if required {
                    self.policies.insert(channel_id, true);
                }
                required
            }
            Err(e) => {
                warn!("room policy lookup failed channel={}: {}", channel_id, e);
                true
            }
        }
    }

    /// 该用户 / 设备在此 Room 是否已被撤权到票据签发之后。查询失败按「已撤销」处理。
    pub async fn is_revoked(
        &self,
        channel_id: u64,
        user_id: u64,
        device_id: &str,
        ticket_iat_secs: u64,
    ) -> bool {
        let rows: std::result::Result<Vec<(i64,)>, _> = sqlx::query_as(
            "SELECT revoked_at FROM privchat_room_access_revocations
              WHERE channel_id = $1 AND user_id = $2 AND device_id IN ($3, '')",
        )
        .bind(channel_id as i64)
        .bind(user_id as i64)
        .bind(device_id)
        .fetch_all(self.pool.as_ref())
        .await;
        match rows {
            Ok(rows) => rows
                .iter()
                .any(|(at,)| issued_before_revocation(ticket_iat_secs, *at)),
            Err(e) => {
                warn!(
                    "room revocation lookup failed channel={} user={}: {}",
                    channel_id, user_id, e
                );
                true
            }
        }
    }

    pub fn subscribe(
        &self,
        session_id: SessionId,
        channel_id: u64,
        grant: RoomGrant,
    ) -> std::result::Result<(), &'static str> {
        let _guard = self.mutation.lock().expect("room grant lock");
        self.subscribe_manager
            .subscribe(session_id.clone(), channel_id)?;
        self.grants
            .entry(session_id)
            .or_default()
            .insert(channel_id, grant);
        Ok(())
    }

    pub fn drop_grant(&self, session_id: &SessionId, channel_id: u64) {
        if let Some(mut g) = self.grants.get_mut(session_id) {
            g.remove(&channel_id);
        }
        self.grants.remove_if(session_id, |_, g| g.is_empty());
    }

    pub fn drop_session(&self, session_id: &SessionId) {
        self.grants.remove(session_id);
    }

    pub fn unsubscribe(&self, session_id: &SessionId, channel_id: u64) {
        let _guard = self.mutation.lock().expect("room grant lock");
        self.subscribe_manager.unsubscribe(session_id, channel_id);
        self.drop_grant(session_id, channel_id);
    }

    pub fn is_locally_current(&self, channel_id: u64, session_id: &SessionId) -> bool {
        let _guard = self.mutation.lock().expect("room grant lock");
        self.subscribe_manager
            .get_session_channels(session_id)
            .contains(&channel_id)
            && self
                .grants
                .get(session_id)
                .and_then(|g| g.get(&channel_id).cloned())
                .is_some_and(|g| g.expires_at > now_secs())
    }

    /// Shared live/replay gate. Revocations are read from the database, not a
    /// process-local cache. A fanout batch uses one query, not one per recipient.
    /// `room_only=false` also permits existing non-Room Transfer subscriptions.
    pub async fn deliverable(
        &self,
        channel_id: u64,
        sessions: Vec<SessionId>,
        room_only: bool,
    ) -> Vec<SessionId> {
        let candidates: Vec<_> = {
            let _guard = self.mutation.lock().expect("room grant lock");
            sessions
                .into_iter()
                .filter_map(|s| {
                    if !self
                        .subscribe_manager
                        .get_session_channels(&s)
                        .contains(&channel_id)
                    {
                        return None;
                    }
                    let grant = self
                        .grants
                        .get(&s)
                        .and_then(|g| g.get(&channel_id).cloned());
                    Some((s, grant))
                })
                .collect()
        };
        if candidates.is_empty() {
            return Vec::new();
        }
        let requires_grant = room_only || self.ticket_required(channel_id).await;
        let users: Vec<i64> = candidates
            .iter()
            .filter_map(|(_, g)| g.as_ref())
            .map(|g| g.user_id as i64)
            .collect();
        let revocations = if users.is_empty() {
            Vec::new()
        } else {
            match sqlx::query_as::<_, (i64, String, i64)>(
                "SELECT user_id, device_id, revoked_at FROM privchat_room_access_revocations
                 WHERE channel_id=$1 AND user_id=ANY($2)",
            )
            .bind(channel_id as i64)
            .bind(&users)
            .fetch_all(self.pool.as_ref())
            .await
            {
                Ok(rows) => rows,
                Err(e) => {
                    warn!(
                        "room delivery authorization unavailable channel={}: {}",
                        channel_id, e
                    );
                    return Vec::new();
                }
            }
        };
        let revoked: HashMap<(u64, String), i64> = revocations
            .into_iter()
            .map(|(u, d, at)| ((u as u64, d), at))
            .collect();
        let _guard = self.mutation.lock().expect("room grant lock");
        let mut result = Vec::new();
        for (session, grant) in candidates {
            if !self
                .subscribe_manager
                .get_session_channels(&session)
                .contains(&channel_id)
                || self
                    .grants
                    .get(&session)
                    .and_then(|g| g.get(&channel_id).cloned())
                    != grant
            {
                continue;
            }
            let valid = match grant {
                Some(ref g) => {
                    g.expires_at > now_secs()
                        && [&g.device_id, &String::new()].into_iter().all(|device| {
                            revoked
                                .get(&(g.user_id, device.clone()))
                                .is_none_or(|at| !issued_before_revocation(g.issued_at, *at))
                        })
                }
                None => !requires_grant,
            };
            if valid {
                result.push(session);
            } else {
                self.subscribe_manager.unsubscribe(&session, channel_id);
                self.drop_grant(&session, channel_id);
            }
        }
        result
    }

    /// 清除已到期的持票订阅。返回清除的 (session, channel) 数量。
    pub fn sweep_expired(&self) -> usize {
        let _guard = self.mutation.lock().expect("room grant lock");
        let now = now_secs();
        let mut expired: Vec<(SessionId, u64)> = Vec::new();
        let mut stale: Vec<(SessionId, u64)> = Vec::new();
        for entry in self.grants.iter() {
            let subscribed = self.subscribe_manager.get_session_channels(entry.key());
            for (channel, grant) in entry.value() {
                if !subscribed.contains(channel) {
                    // 已退订或断线：只清理授权记录。
                    stale.push((entry.key().clone(), *channel));
                } else if grant.expires_at <= now {
                    expired.push((entry.key().clone(), *channel));
                }
            }
        }
        for (session, channel) in &stale {
            self.drop_grant(session, *channel);
        }
        for (session, channel) in &expired {
            self.subscribe_manager.unsubscribe(session, *channel);
            self.drop_grant(session, *channel);
        }
        if !expired.is_empty() {
            info!(
                "room access: {} expired ticket subscriptions removed",
                expired.len()
            );
        }
        expired.len()
    }

    /// 撤权：先持久化，再清除该用户（指定设备或全部设备）在此 Room 的现有订阅。
    /// 返回被清除的会话数。只影响这一个 Room，不踢掉该用户其他 Room 或连接。
    pub async fn revoke(
        &self,
        channel_id: u64,
        user_id: u64,
        device_id: Option<&str>,
    ) -> Result<usize> {
        self.revoke_before(channel_id, user_id, device_id, now_ms())
            .await
    }

    /// Business retries carry their original cutoff; they must not revoke a
    /// later re-authorization just because the delivery was delayed.
    pub async fn revoke_before(
        &self,
        channel_id: u64,
        user_id: u64,
        device_id: Option<&str>,
        revoked_before_ms: i64,
    ) -> Result<usize> {
        if revoked_before_ms < 0 {
            return Err(ServerError::Validation(
                "revoked_before_ms must be nonnegative".into(),
            ));
        }
        let device = device_id.unwrap_or("");
        let (cutoff,): (i64,) = sqlx::query_as(
            "INSERT INTO privchat_room_access_revocations (channel_id, user_id, device_id, revoked_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (channel_id, user_id, device_id) DO UPDATE SET revoked_at =
                 GREATEST(privchat_room_access_revocations.revoked_at, EXCLUDED.revoked_at)
             RETURNING revoked_at",
        )
        .bind(channel_id as i64)
        .bind(user_id as i64)
        .bind(device)
        .bind(revoked_before_ms)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("room revocation insert: {e}")))?;

        let _guard = self.mutation.lock().expect("room grant lock");
        let sessions: Vec<_> = self
            .grants
            .iter()
            .filter_map(|entry| {
                entry
                    .value()
                    .get(&channel_id)
                    .filter(|g| {
                        g.user_id == user_id
                            && (device.is_empty() || g.device_id == device)
                            && issued_before_revocation(g.issued_at, cutoff)
                    })
                    .map(|_| entry.key().clone())
            })
            .collect();
        let mut removed = 0usize;
        for session in sessions {
            removed += usize::from(self.subscribe_manager.unsubscribe(&session, channel_id));
            self.drop_grant(&session, channel_id);
        }
        info!(
            "room access revoked channel={} user={} device='{}' sessions_removed={}",
            channel_id, user_id, device, removed
        );
        Ok(removed)
    }

    /// 后台到期清理。
    pub fn spawn_sweeper(self: &Arc<Self>, interval: std::time::Duration) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                me.sweep_expired();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tickets_issued_at_or_before_revocation_are_revoked() {
        assert!(issued_before_revocation(u64::MAX, 1_000_000));
        assert!(issued_before_revocation(1_000, 1_000_000));
        assert!(
            issued_before_revocation(1_000, 1_000_500),
            "同一秒签发无法证明晚于撤权"
        );
        assert!(!issued_before_revocation(1_001, 1_000_500));
    }

    async fn service() -> Option<RoomAccessService> {
        let url = crate::require_test_database_url()?;
        let pool = Arc::new(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(&url)
                .await
                .expect("connect"),
        );
        Some(RoomAccessService::new(
            pool,
            Arc::new(SubscribeManager::new()),
            Arc::new(ConnectionManager::new()),
        ))
    }

    #[tokio::test]
    async fn policy_and_revocation_persist() {
        let _guard = crate::database_fixture_lock().lock().await;
        let Some(svc) = service().await else { return };
        let channel = 9_400_000_000 + (now_ms() as u64 % 1_000_000);
        assert!(
            !svc.ticket_required(channel).await,
            "未登记的 Room 保持原兼容行为"
        );
        svc.set_ticket_required(channel, true).await.unwrap();
        assert!(svc.ticket_required(channel).await);

        let before = now_secs() - 5;
        assert!(!svc.is_revoked(channel, 7, "dev-a", before).await);
        svc.revoke(channel, 7, Some("dev-a")).await.unwrap();
        assert!(
            svc.is_revoked(channel, 7, "dev-a", before).await,
            "撤权前签发的票据失效"
        );
        assert!(
            !svc.is_revoked(channel, 7, "dev-b", before).await,
            "只撤这一台设备"
        );
        assert!(
            !svc.is_revoked(channel, 8, "dev-a", before).await,
            "不影响其他用户"
        );
        assert!(
            !svc.is_revoked(channel, 7, "dev-a", now_secs() + 2).await,
            "撤权后重新签发的票据有效"
        );
        svc.revoke(channel, 7, None).await.unwrap();
        assert!(
            svc.is_revoked(channel, 7, "dev-b", before).await,
            "全设备撤权"
        );
    }

    fn peer(svc: &RoomAccessService) -> RoomAccessService {
        RoomAccessService::new(
            svc.pool.clone(),
            Arc::new(SubscribeManager::new()),
            Arc::new(ConnectionManager::new()),
        )
    }

    fn grant(user_id: u64, device: &str) -> RoomGrant {
        RoomGrant {
            user_id,
            device_id: device.into(),
            expires_at: now_secs() + 300,
            issued_at: now_secs().saturating_sub(5),
        }
    }

    async fn fresh_channel(svc: &RoomAccessService, suffix: u64) -> u64 {
        let channel = 9_500_000_000 + suffix;
        sqlx::query("DELETE FROM privchat_room_access_revocations WHERE channel_id=$1")
            .bind(channel as i64)
            .execute(svc.pool.as_ref())
            .await
            .unwrap();
        sqlx::query("DELETE FROM privchat_room_policies WHERE channel_id=$1")
            .bind(channel as i64)
            .execute(svc.pool.as_ref())
            .await
            .unwrap();
        channel
    }

    #[tokio::test]
    async fn another_server_revocation_stops_existing_delivery_without_sweeper() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let remote = peer(&svc);
        let channel = fresh_channel(&svc, 1).await;
        let a = SessionId::from(94001);
        let b = SessionId::from(94002);
        remote.subscribe(a.clone(), channel, grant(7, "a")).unwrap();
        remote.subscribe(b.clone(), channel, grant(7, "b")).unwrap();
        assert_eq!(
            remote
                .deliverable(channel, vec![a.clone(), b.clone()], true)
                .await
                .len(),
            2
        );
        svc.revoke(channel, 7, Some("a")).await.unwrap();
        assert_eq!(
            remote
                .deliverable(channel, vec![a.clone(), b.clone()], true)
                .await,
            vec![b]
        );
        assert!(!remote
            .subscribe_manager
            .get_session_channels(&a)
            .contains(&channel));
    }

    #[tokio::test]
    async fn revocation_between_validation_and_registration_cannot_restore_access() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let remote = peer(&svc);
        let channel = fresh_channel(&svc, 2).await;
        let s = SessionId::from(94003);
        let g = grant(7, "a");
        assert!(!remote.is_revoked(channel, 7, "a", g.issued_at).await);
        svc.revoke(channel, 7, Some("a")).await.unwrap();
        remote.subscribe(s.clone(), channel, g).unwrap();
        assert!(remote.deliverable(channel, vec![s], true).await.is_empty());
    }

    #[tokio::test]
    async fn stale_fanout_and_missing_grant_cannot_become_legacy_authorization() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let channel = fresh_channel(&svc, 3).await;
        svc.set_ticket_required(channel, true).await.unwrap();
        let s = SessionId::from(94004);
        svc.subscribe(s.clone(), channel, grant(7, "a")).unwrap();
        let queued = vec![s.clone()];
        svc.revoke(channel, 7, None).await.unwrap();
        assert!(svc.deliverable(channel, queued, true).await.is_empty());
        // Even a subscription introduced outside the Room handler cannot skip its grant.
        svc.subscribe_manager.subscribe(s.clone(), channel).unwrap();
        assert!(svc.deliverable(channel, vec![s], false).await.is_empty());
    }

    #[tokio::test]
    async fn replay_gate_checks_expiry_without_waiting_for_cleanup() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let channel = fresh_channel(&svc, 4).await;
        let s = SessionId::from(94005);
        let mut expired = grant(7, "a");
        expired.expires_at = now_secs();
        svc.subscribe(s.clone(), channel, expired).unwrap();
        assert!(svc
            .deliverable(channel, vec![s.clone()], true)
            .await
            .is_empty());
        svc.subscribe(s.clone(), channel, grant(7, "a")).unwrap();
        assert_eq!(svc.sweep_expired(), 0);
        assert_eq!(
            svc.deliverable(channel, vec![s.clone()], true).await,
            vec![s]
        );
    }

    #[tokio::test]
    async fn missing_policy_is_not_cached_across_server_creation() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let remote = peer(&svc);
        let channel = fresh_channel(&svc, 5).await;
        assert!(!remote.ticket_required(channel).await);
        svc.set_ticket_required(channel, true).await.unwrap();
        assert!(remote.ticket_required(channel).await);
    }

    #[tokio::test]
    async fn revocation_store_failure_never_means_unrevoked() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let channel = fresh_channel(&svc, 6).await;
        let s = SessionId::from(94006);
        svc.subscribe(s.clone(), channel, grant(7, "a")).unwrap();
        svc.pool.close().await;
        assert!(svc.deliverable(channel, vec![s], true).await.is_empty());
        assert!(svc.ticket_required(channel).await);
    }

    #[tokio::test]
    async fn retrying_old_revocation_preserves_new_grant_and_never_regresses_cutoff() {
        let _guard = crate::database_fixture_lock().lock().await;
        let svc = service().await.unwrap();
        let channel = fresh_channel(&svc, 7).await;
        let cutoff = (now_secs().saturating_sub(10) * 1000) as i64;
        svc.revoke_before(channel, 7, Some("a"), cutoff)
            .await
            .unwrap();
        let s = SessionId::from(94007);
        svc.subscribe(s.clone(), channel, grant(7, "a")).unwrap();
        assert_eq!(
            svc.revoke_before(channel, 7, Some("a"), cutoff)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            svc.deliverable(channel, vec![s.clone()], true).await,
            vec![s]
        );
        svc.revoke_before(channel, 7, Some("a"), cutoff - 10_000)
            .await
            .unwrap();
        let (stored,): (i64,) = sqlx::query_as(
            "SELECT revoked_at FROM privchat_room_access_revocations WHERE channel_id=$1 AND user_id=7 AND device_id='a'"
        ).bind(channel as i64).fetch_one(svc.pool.as_ref()).await.unwrap();
        assert_eq!(stored, cutoff);
    }
}
