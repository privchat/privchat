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
use std::sync::Arc;
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
}

pub struct RoomAccessService {
    pool: Arc<PgPool>,
    subscribe_manager: Arc<SubscribeManager>,
    connection_manager: Arc<ConnectionManager>,
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
    (ticket_iat_secs as i64).saturating_mul(1000) <= revoked_at_ms
}

impl RoomAccessService {
    pub fn new(
        pool: Arc<PgPool>,
        subscribe_manager: Arc<SubscribeManager>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            pool,
            subscribe_manager,
            connection_manager,
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
                self.policies.insert(channel_id, required);
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

    pub fn record_grant(&self, session_id: SessionId, channel_id: u64, grant: RoomGrant) {
        self.grants
            .entry(session_id)
            .or_default()
            .insert(channel_id, grant);
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

    /// 投递前过滤：持票订阅已到期的会话不再接收（清理由 [sweep_expired] 完成）。
    pub fn deliverable(&self, channel_id: u64, sessions: Vec<SessionId>) -> Vec<SessionId> {
        let now = now_secs();
        sessions
            .into_iter()
            .filter(
                |s| match self.grants.get(s).and_then(|g| g.get(&channel_id).cloned()) {
                    Some(grant) => grant.expires_at > now,
                    None => true,
                },
            )
            .collect()
    }

    /// 清除已到期的持票订阅。返回清除的 (session, channel) 数量。
    pub fn sweep_expired(&self) -> usize {
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
        let device = device_id.unwrap_or("");
        sqlx::query(
            "INSERT INTO privchat_room_access_revocations (channel_id, user_id, device_id, revoked_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (channel_id, user_id, device_id) DO UPDATE SET revoked_at = EXCLUDED.revoked_at",
        )
        .bind(channel_id as i64)
        .bind(user_id as i64)
        .bind(device)
        .bind(now_ms())
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("room revocation insert: {e}")))?;

        let mut removed = 0usize;
        for conn in self.connection_manager.get_user_connections(user_id).await {
            if !device.is_empty() && conn.device_id != device {
                continue;
            }
            if self
                .subscribe_manager
                .unsubscribe(&conn.session_id, channel_id)
            {
                removed += 1;
            }
            self.drop_grant(&conn.session_id, channel_id);
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
}
