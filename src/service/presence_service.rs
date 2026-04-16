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

use msgtrans::{packet::Packet, SessionId};
#[cfg(test)]
use sqlx::postgres::PgPoolOptions;
use std::sync::Arc;
#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;
use tracing::{debug, warn};

use crate::error::ServerError;
use crate::infra::{ConnectionManager, PresenceTracker, SubscribeManager};
#[cfg(test)]
use crate::repository::PgChannelRepository;
use crate::service::ChannelService;
use privchat_protocol::presence::{
    PresenceBatchStatusResponse, PresenceChangedNotification, PresenceSnapshot,
};
use privchat_protocol::protocol::{MessageType, PublishRequest};

/// PresenceService
///
/// 职责：
/// 1. 对外提供按 user_ids 查询当前 Presence 的能力
/// 2. 接收 connect / disconnect / heartbeat / timeout 生命周期事实
/// 3. 将某个 user_id 的 Presence 变化发布到相关 channelId 的实时事件流
pub struct PresenceService {
    presence_tracker: Arc<PresenceTracker>,
    channel_service: Arc<ChannelService>,
    subscribe_manager: Arc<SubscribeManager>,
    connection_manager: Arc<ConnectionManager>,
    #[cfg(test)]
    test_visibility_overrides: Mutex<HashMap<(u64, u64), bool>>,
    #[cfg(test)]
    test_presence_channels: Mutex<HashMap<u64, Vec<u64>>>,
    #[cfg(test)]
    test_published_events: Mutex<Vec<(u64, PresenceChangedNotification)>>,
}

impl PresenceService {
    pub fn new(
        presence_tracker: Arc<PresenceTracker>,
        channel_service: Arc<ChannelService>,
        subscribe_manager: Arc<SubscribeManager>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            presence_tracker,
            channel_service,
            subscribe_manager,
            connection_manager,
            #[cfg(test)]
            test_visibility_overrides: Mutex::new(HashMap::new()),
            #[cfg(test)]
            test_presence_channels: Mutex::new(HashMap::new()),
            #[cfg(test)]
            test_published_events: Mutex::new(Vec::new()),
        }
    }

    /// 按 user_ids 获取当前在线状态。
    ///
    /// 权限判断基于关系/会话上下文，但不改变查询主键：
    /// - 自己总是可见
    /// - 与当前 viewer 已存在合法直聊 channel 的用户可见
    pub async fn batch_get_status(
        &self,
        viewer_user_id: u64,
        user_ids: Vec<u64>,
    ) -> PresenceBatchStatusResponse {
        let mut allowed_ids = Vec::new();
        let mut denied_user_ids = Vec::new();

        for target_user_id in user_ids {
            if target_user_id == 0 {
                continue;
            }

            if target_user_id == viewer_user_id
                || self.can_view_target(viewer_user_id, target_user_id).await
            {
                allowed_ids.push(target_user_id);
            } else {
                denied_user_ids.push(target_user_id);
            }
        }

        let items = self.presence_tracker.batch_get_snapshots(allowed_ids).await;

        PresenceBatchStatusResponse {
            items,
            denied_user_ids,
        }
    }

    pub async fn on_device_connected(
        &self,
        user_id: u64,
        device_id: impl Into<String>,
    ) -> Result<(), ServerError> {
        let snapshot = self
            .presence_tracker
            .on_device_connected(user_id, device_id)
            .await?;
        self.publish_presence_changed(snapshot).await
    }

    pub async fn on_device_disconnected(
        &self,
        user_id: u64,
        device_id: &str,
    ) -> Result<(), ServerError> {
        let snapshot = self
            .presence_tracker
            .on_device_disconnected(user_id, device_id)
            .await?;
        self.publish_presence_changed(snapshot).await
    }

    pub async fn on_heartbeat(&self, user_id: u64) -> Result<(), ServerError> {
        self.presence_tracker.on_heartbeat(user_id).await
    }

    pub async fn on_timeout(&self, user_id: u64) -> Result<(), ServerError> {
        let snapshot = self.presence_tracker.on_timeout(user_id).await?;
        self.publish_presence_changed(snapshot).await
    }

    async fn publish_presence_changed(&self, snapshot: PresenceSnapshot) -> Result<(), ServerError> {
        let user_id = snapshot.user_id;
        let related_channels = self.list_presence_channels_for_user(user_id).await?;

        if related_channels.is_empty() {
            return Ok(());
        }

        let payload = PresenceChangedNotification {
            user_id,
            version: snapshot.version,
            snapshot,
        };

        #[cfg(test)]
        {
            let mut published = self.test_published_events.lock().unwrap();
            for channel_id in &related_channels {
                published.push((*channel_id, payload.clone()));
            }
        }

        let payload_bytes = serde_json::to_vec(&payload).map_err(|e| {
            ServerError::Internal(format!("Serialize presence_changed payload failed: {}", e))
        })?;

        let transport = self.connection_manager.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            return Ok(());
        };

        for channel_id in related_channels {
            let publish_request = PublishRequest {
                channel_id,
                topic: Some("presence_changed".to_string()),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                payload: payload_bytes.clone(),
                publisher: Some(user_id.to_string()),
                server_message_id: None,
            };

            let encoded = privchat_protocol::encode_message(&publish_request).map_err(|e| {
                ServerError::Protocol(format!("Encode presence_changed publish failed: {}", e))
            })?;

            let sessions = self.subscribe_manager.get_channel_sessions(channel_id);
            if sessions.is_empty() {
                continue;
            }

            let mut delivered = 0usize;
            for session_id in sessions {
                let mut packet = Packet::one_way(crate::infra::next_packet_id(), encoded.clone());
                packet.set_biz_type(MessageType::PublishRequest as u8);
                match server.send_to_session(session_id.clone(), packet).await {
                    Ok(()) => delivered += 1,
                    Err(e) => {
                        warn!(
                            "⚠️ PresenceService: failed to publish presence_changed to session {} for channel {}: {}",
                            session_id, channel_id, e
                        );
                        self.subscribe_manager.unsubscribe(&session_id, channel_id);
                    }
                }
            }

            debug!(
                "📢 PresenceService: published presence_changed user={} channel={} delivered={}",
                user_id, channel_id, delivered
            );
        }

        Ok(())
    }

    pub async fn on_heartbeat_by_session(&self, session_id: &SessionId) -> Result<(), ServerError> {
        if let Some(connection) = self.connection_manager.get_connection_by_session(session_id).await
        {
            self.on_heartbeat(connection.user_id).await?;
        }
        Ok(())
    }

    async fn can_view_target(&self, viewer_user_id: u64, target_user_id: u64) -> bool {
        #[cfg(test)]
        if let Some(allowed) = self
            .test_visibility_overrides
            .lock()
            .unwrap()
            .get(&(viewer_user_id, target_user_id))
            .copied()
        {
            return allowed;
        }

        self.channel_service
            .has_presence_context(viewer_user_id, target_user_id)
            .await
    }

    async fn list_presence_channels_for_user(&self, user_id: u64) -> Result<Vec<u64>, ServerError> {
        #[cfg(test)]
        if let Some(channels) = self
            .test_presence_channels
            .lock()
            .unwrap()
            .get(&user_id)
            .cloned()
        {
            return Ok(channels);
        }

        self.channel_service.list_presence_channels_for_user(user_id).await
    }

    #[cfg(test)]
    fn set_test_visibility(&self, viewer_user_id: u64, target_user_id: u64, allowed: bool) {
        self.test_visibility_overrides
            .lock()
            .unwrap()
            .insert((viewer_user_id, target_user_id), allowed);
    }

    #[cfg(test)]
    fn set_test_presence_channels(&self, user_id: u64, channel_ids: Vec<u64>) {
        self.test_presence_channels
            .lock()
            .unwrap()
            .insert(user_id, channel_ids);
    }

    #[cfg(test)]
    fn take_test_published_events(&self) -> Vec<(u64, PresenceChangedNotification)> {
        std::mem::take(&mut *self.test_published_events.lock().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::PresenceStateStore;

    fn test_channel_service() -> Arc<ChannelService> {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://privchat:privchat@localhost/privchat_test")
            .expect("lazy pg pool");
        let repo = Arc::new(PgChannelRepository::new(Arc::new(pool)));
        Arc::new(ChannelService::new_with_repository(repo))
    }

    fn test_service() -> PresenceService {
        let state_store = PresenceStateStore::with_default_config(None);
        let presence_tracker = Arc::new(PresenceTracker::new(state_store));
        let channel_service = test_channel_service();
        let subscribe_manager = Arc::new(SubscribeManager::new());
        let connection_manager = Arc::new(ConnectionManager::new());

        PresenceService::new(
            presence_tracker,
            channel_service,
            subscribe_manager,
            connection_manager,
        )
    }

    #[tokio::test]
    async fn batch_status_filters_denied_targets() {
        let service = test_service();

        service.set_test_visibility(7, 8, true);
        service.set_test_visibility(7, 9, false);
        service
            .presence_tracker
            .on_device_connected(8, "ios-8")
            .await
            .unwrap();

        let response = service.batch_get_status(7, vec![7, 8, 9, 0]).await;

        let item_user_ids = response.items.iter().map(|item| item.user_id).collect::<Vec<_>>();
        assert_eq!(item_user_ids, vec![7, 8]);
        assert_eq!(response.denied_user_ids, vec![9]);
    }

    #[tokio::test]
    async fn batch_status_returns_stable_empty_result() {
        let service = test_service();

        service.set_test_visibility(10, 11, false);
        service.set_test_visibility(10, 12, false);

        let response = service.batch_get_status(10, vec![0, 11, 12]).await;

        assert!(response.items.is_empty());
        assert_eq!(response.denied_user_ids, vec![11, 12]);
    }

    #[tokio::test]
    async fn presence_changed_fanouts_to_eligible_channels_only() {
        let service = test_service();

        service.set_test_presence_channels(42, vec![100, 200]);
        service.subscribe_manager.subscribe(SessionId::from(1_u64), 100).unwrap();
        service.subscribe_manager.subscribe(SessionId::from(2_u64), 200).unwrap();
        service.subscribe_manager.subscribe(SessionId::from(3_u64), 300).unwrap();

        service.on_device_connected(42, "ios-42").await.unwrap();

        let published = service.take_test_published_events();
        let channel_ids = published
            .iter()
            .map(|(channel_id, _)| *channel_id)
            .collect::<Vec<_>>();

        assert_eq!(channel_ids, vec![100, 200]);
        assert!(published.iter().all(|(_, payload)| payload.user_id == 42));
        assert!(published
            .iter()
            .all(|(_, payload)| payload.snapshot.is_online && payload.snapshot.device_count == 1));
    }

    #[tokio::test]
    async fn heartbeat_does_not_publish_presence_changed() {
        let service = test_service();

        service.set_test_presence_channels(66, vec![888]);
        service.subscribe_manager.subscribe(SessionId::from(4_u64), 888).unwrap();

        service.on_device_connected(66, "ios-66").await.unwrap();
        let _ = service.take_test_published_events();

        service.on_heartbeat(66).await.unwrap();

        let published = service.take_test_published_events();
        assert!(published.is_empty());
    }

    #[tokio::test]
    async fn timeout_marks_offline_and_fanouts_to_related_channels() {
        let service = test_service();

        service.set_test_presence_channels(77, vec![901, 902]);
        service.subscribe_manager.subscribe(SessionId::from(11_u64), 901).unwrap();
        service.subscribe_manager.subscribe(SessionId::from(12_u64), 902).unwrap();

        service.on_device_connected(77, "ios-77").await.unwrap();
        let _ = service.take_test_published_events();

        service.on_timeout(77).await.unwrap();

        let published = service.take_test_published_events();
        assert_eq!(published.len(), 2);
        assert!(published.iter().all(|(_, payload)| payload.user_id == 77));
        assert!(published
            .iter()
            .all(|(_, payload)| !payload.snapshot.is_online && payload.snapshot.device_count == 0));
    }

    #[tokio::test]
    async fn query_snapshot_matches_notified_snapshot() {
        let service = test_service();

        service.set_test_visibility(5, 88, true);
        service.set_test_presence_channels(88, vec![555]);
        service.subscribe_manager.subscribe(SessionId::from(9_u64), 555).unwrap();

        service.on_device_connected(88, "ios-88").await.unwrap();

        let response = service.batch_get_status(5, vec![88]).await;
        let queried = response.items.into_iter().next().expect("queried snapshot");
        let published = service.take_test_published_events();
        let notified = &published[0].1.snapshot;

        assert_eq!(queried.user_id, notified.user_id);
        assert_eq!(queried.is_online, notified.is_online);
        assert_eq!(queried.device_count, notified.device_count);
        assert_eq!(queried.version, notified.version);
    }

    /// 验证：当用户会话因超时被清理时，PresenceService.on_timeout 会正确发布离线状态给订阅者
    /// 这是 privchat-iced 能够收到对方离线状态更新的关键路径
    #[tokio::test]
    async fn cleanup_timeout_publishes_offline_to_subscribers() {
        let service = test_service();

        // 设置用户 100 的订阅者
        service.set_test_visibility(200, 100, true);
        service.set_test_presence_channels(100, vec![3001]);
        service.subscribe_manager.subscribe(SessionId::from(200_u64), 3001).unwrap();

        // 用户 100 上线
        service.on_device_connected(100, "device-100").await.unwrap();
        let _ = service.take_test_published_events(); // 清除上线事件

        // 模拟会话超时清理（这应该是服务器清理过期会话时调用的路径）
        service.on_timeout(100).await.unwrap();

        // 验证订阅者收到了离线状态更新
        let published = service.take_test_published_events();
        assert_eq!(published.len(), 1);

        let (channel_id, notification) = &published[0];
        assert_eq!(*channel_id, 3001);
        assert_eq!(notification.user_id, 100);
        assert!(!notification.snapshot.is_online, "用户应该被标记为离线");
        assert_eq!(notification.snapshot.device_count, 0, "设备数应该为 0");
        assert!(notification.version > 0, "version 应该递增");
    }

    /// 验证：用户主动登出（logout）时，on_device_disconnected 会正确发布离线状态给订阅者
    /// 这是 account/auth/logout RPC 的关键路径，确保订阅者能看到用户下线
    #[tokio::test]
    async fn logout_publishes_offline_to_subscribers() {
        let service = test_service();

        // 设置用户 200 的订阅者（用户 300 在关注 200 的状态）
        service.set_test_visibility(300, 200, true);
        service.set_test_presence_channels(200, vec![4001]);
        service.subscribe_manager.subscribe(SessionId::from(300_u64), 4001).unwrap();

        // 用户 200 上线（模拟登录成功）
        service.on_device_connected(200, "device-200").await.unwrap();
        let _ = service.take_test_published_events(); // 清除上线事件

        // 用户点击登出（调用 on_device_disconnected，与 transport close 路径相同）
        service.on_device_disconnected(200, "device-200").await.unwrap();

        // 验证订阅者收到了离线状态更新
        let published = service.take_test_published_events();
        assert_eq!(published.len(), 1);

        let (channel_id, notification) = &published[0];
        assert_eq!(*channel_id, 4001);
        assert_eq!(notification.user_id, 200);
        assert!(!notification.snapshot.is_online, "登出后用户应该被标记为离线");
        assert_eq!(notification.snapshot.device_count, 0, "登出后设备数应该为 0");
        assert!(notification.version > 1, "登出后 version 应该递增");
    }
}
