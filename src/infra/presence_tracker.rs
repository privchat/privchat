// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
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

use dashmap::DashMap;
use std::collections::HashSet;
use std::sync::Arc;

use crate::error::ServerError;
use crate::infra::PresenceStateStore;
use privchat_protocol::presence::{OnlineStatusInfo, PresenceSnapshot};

/// PresenceTracker
///
/// 负责：
/// 1. 维护 per-user monotonic version
/// 2. 将底层 PresenceStateStore 的运行态转换为对外快照
/// 3. 承接生命周期事实（connect / disconnect / heartbeat / timeout）
pub struct PresenceTracker {
    state_store: Arc<PresenceStateStore>,
    versions: DashMap<u64, u64>,
    online_devices: DashMap<u64, HashSet<String>>,
}

impl PresenceTracker {
    pub fn new(state_store: Arc<PresenceStateStore>) -> Self {
        Self {
            state_store,
            versions: DashMap::new(),
            online_devices: DashMap::new(),
        }
    }

    pub async fn batch_get_snapshots(&self, user_ids: Vec<u64>) -> Vec<PresenceSnapshot> {
        let statuses = self.state_store.batch_get_status(user_ids).await;
        let mut snapshots = Vec::new();
        let mut ordered = statuses.into_iter().collect::<Vec<_>>();
        ordered.sort_by_key(|(user_id, _)| *user_id);

        for (_, status) in ordered {
            snapshots.push(self.to_snapshot(status));
        }

        snapshots
    }

    pub async fn get_snapshot(&self, user_id: u64) -> PresenceSnapshot {
        let status = self.state_store.get_status_with_db(user_id).await;
        self.to_snapshot(status)
    }

    pub async fn on_device_connected(
        &self,
        user_id: u64,
        device_id: impl Into<String>,
    ) -> Result<PresenceSnapshot, ServerError> {
        let old_snapshot = self.get_snapshot(user_id).await;
        self.add_online_device(user_id, device_id.into());
        self.state_store.user_online(user_id).await?;
        let mut snapshot = self.get_snapshot(user_id).await;
        snapshot.version = self.version_after_change(user_id, &old_snapshot, &snapshot);
        Ok(snapshot)
    }

    pub async fn on_device_disconnected(
        &self,
        user_id: u64,
        device_id: &str,
    ) -> Result<PresenceSnapshot, ServerError> {
        let old_snapshot = self.get_snapshot(user_id).await;
        let remaining_devices = self.remove_online_device(user_id, device_id);

        if remaining_devices == 0 {
            self.state_store.user_offline(user_id).await?;
        } else {
            self.state_store.update_heartbeat(user_id).await?;
        }

        let mut snapshot = self.get_snapshot(user_id).await;
        snapshot.version = self.version_after_change(user_id, &old_snapshot, &snapshot);
        Ok(snapshot)
    }

    pub async fn on_heartbeat(&self, user_id: u64) -> Result<(), ServerError> {
        self.state_store.update_heartbeat(user_id).await
    }

    pub async fn on_timeout(&self, user_id: u64) -> Result<PresenceSnapshot, ServerError> {
        let old_snapshot = self.get_snapshot(user_id).await;
        self.online_devices.remove(&user_id);
        self.state_store.user_offline(user_id).await?;
        let mut snapshot = self.get_snapshot(user_id).await;
        snapshot.version = self.version_after_change(user_id, &old_snapshot, &snapshot);
        Ok(snapshot)
    }

    pub fn current_version(&self, user_id: u64) -> u64 {
        self.versions.get(&user_id).map(|v| *v).unwrap_or(0)
    }

    fn to_snapshot(&self, status: OnlineStatusInfo) -> PresenceSnapshot {
        let device_count = self.device_count(status.user_id);
        PresenceSnapshot {
            user_id: status.user_id,
            is_online: device_count > 0,
            last_seen_at: status.last_seen,
            device_count,
            version: self.current_version(status.user_id),
        }
    }

    fn bump_version(&self, user_id: u64) -> u64 {
        let mut entry = self.versions.entry(user_id).or_insert(0);
        *entry += 1;
        *entry
    }

    fn version_after_change(
        &self,
        user_id: u64,
        old_snapshot: &PresenceSnapshot,
        new_snapshot: &PresenceSnapshot,
    ) -> u64 {
        if self.visible_state_changed(old_snapshot, new_snapshot) {
            self.bump_version(user_id)
        } else {
            self.current_version(user_id)
        }
    }

    fn visible_state_changed(
        &self,
        old_snapshot: &PresenceSnapshot,
        new_snapshot: &PresenceSnapshot,
    ) -> bool {
        old_snapshot.is_online != new_snapshot.is_online
            || old_snapshot.device_count != new_snapshot.device_count
    }

    fn add_online_device(&self, user_id: u64, device_id: String) {
        let mut devices = self
            .online_devices
            .entry(user_id)
            .or_insert_with(HashSet::new);
        devices.insert(device_id);
    }

    fn remove_online_device(&self, user_id: u64, device_id: &str) -> usize {
        if let Some(mut devices) = self.online_devices.get_mut(&user_id) {
            devices.remove(device_id);
            let remaining = devices.len();
            if remaining == 0 {
                drop(devices);
                self.online_devices.remove(&user_id);
            }
            remaining
        } else {
            0
        }
    }

    fn device_count(&self, user_id: u64) -> u32 {
        self.online_devices
            .get(&user_id)
            .map(|devices| devices.len() as u32)
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::PresenceStateStore;

    fn tracker() -> PresenceTracker {
        let state_store = PresenceStateStore::with_default_config(None);
        PresenceTracker::new(state_store)
    }

    #[tokio::test]
    async fn connect_creates_presence_and_bumps_version() {
        let tracker = tracker();

        let snapshot = tracker.on_device_connected(1001, "ios-1").await.unwrap();

        assert!(snapshot.is_online);
        assert_eq!(snapshot.device_count, 1);
        assert_eq!(snapshot.version, 1);
        assert!(snapshot.last_seen_at > 0);
    }

    #[tokio::test]
    async fn heartbeat_does_not_bump_version_when_visible_state_unchanged() {
        let tracker = tracker();

        let snapshot = tracker.on_device_connected(1002, "ios-1").await.unwrap();
        let version = snapshot.version;
        let last_seen = snapshot.last_seen_at;

        tracker.on_heartbeat(1002).await.unwrap();
        let after = tracker.get_snapshot(1002).await;

        assert_eq!(after.version, version);
        assert!(after.last_seen_at >= last_seen);
        assert!(after.is_online);
        assert_eq!(after.device_count, 1);
    }

    #[tokio::test]
    async fn disconnect_last_device_marks_offline_and_preserves_last_seen() {
        let tracker = tracker();

        let online = tracker.on_device_connected(1003, "ios-1").await.unwrap();
        let before_last_seen = online.last_seen_at;

        let offline = tracker
            .on_device_disconnected(1003, "ios-1")
            .await
            .unwrap();

        assert!(!offline.is_online);
        assert_eq!(offline.device_count, 0);
        assert_eq!(offline.version, 2);
        assert!(offline.last_seen_at >= before_last_seen);
    }

    #[tokio::test]
    async fn multi_device_disconnect_only_changes_device_count_until_final_offline() {
        let tracker = tracker();

        let first = tracker.on_device_connected(1004, "ios-1").await.unwrap();
        assert_eq!(first.device_count, 1);
        assert_eq!(first.version, 1);

        let second = tracker.on_device_connected(1004, "web-1").await.unwrap();
        assert!(second.is_online);
        assert_eq!(second.device_count, 2);
        assert_eq!(second.version, 2);

        let one_left = tracker
            .on_device_disconnected(1004, "ios-1")
            .await
            .unwrap();
        assert!(one_left.is_online);
        assert_eq!(one_left.device_count, 1);
        assert_eq!(one_left.version, 3);

        let offline = tracker
            .on_device_disconnected(1004, "web-1")
            .await
            .unwrap();
        assert!(!offline.is_online);
        assert_eq!(offline.device_count, 0);
        assert_eq!(offline.version, 4);
    }

    #[tokio::test]
    async fn timeout_marks_user_offline_and_bumps_version() {
        let tracker = tracker();

        let online = tracker.on_device_connected(1005, "ios-1").await.unwrap();
        assert!(online.is_online);
        assert_eq!(online.version, 1);

        let timeout = tracker.on_timeout(1005).await.unwrap();
        assert!(!timeout.is_online);
        assert_eq!(timeout.device_count, 0);
        assert_eq!(timeout.version, 2);
        assert!(timeout.last_seen_at >= online.last_seen_at);
    }
}
