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

//! Web 扫码登录的实时推送通道 —— `scene_id` 与发起这条 scene 的 unauth WebSocket
//! session 的双向 registry（spec QR_API §5）。
//!
//! 设计要点：
//!
//! - **Ephemeral**：仅内存，不持久化。server 重启 = 客户端重新建连+重生成二维码。
//! - **1 unauth session = 1 active scene**。同一 session 创建第二个 scene 会替换第一个，
//!   旧 scene 不再收到推送（mobile 后续 scan/confirm 会回 NotFound 或 Stale 状态）。
//! - **双向索引**：`scene_to_session` + `session_to_scene`，让 connection close 能在
//!   常数时间内找出待清理的 scene。
//! - **NoSubscriber 不是错误**：`push_event` 返回 `Delivered` / `NoSubscriber`，业务侧
//!   绝不能因为 NoSubscriber 回滚 confirm（Web 已经断开是常态）。
//!
//! 推送实现：把事件 JSON 编码进 `PushMessageRequest.payload`，topic 设为 `qr_login.<event>`，
//! 调 `ConnectionManager::send_unauth_event_to_session()` 绕过 Authenticated 闸门下发。

use std::sync::Arc;

use dashmap::DashMap;
use msgtrans::SessionId;
use privchat_protocol::protocol::PushMessageRequest;
use privchat_protocol::rpc::qr_login::QrLoginPushEvent;

use crate::infra::ConnectionManager;

/// 推送结果，用于让上层区分「真的下发了」和「Web 已经断开」两种情况。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// 找到 binding，且 transport 调用成功（≥1 帧已写入网络）。
    Delivered,
    /// 没有 binding 或 binding 已失效（Web 早断开 / 没绑过）。**不是错误**。
    NoSubscriber,
}

#[derive(Clone)]
pub struct QrLoginPublisher {
    inner: Arc<Inner>,
}

struct Inner {
    /// `scene_id → session_id`：transition 后查找推送目标。
    scene_to_session: DashMap<String, SessionId>,
    /// `session_id → scene_id`：connection close 时常数时间清理。
    session_to_scene: DashMap<SessionId, String>,
}

impl Default for QrLoginPublisher {
    fn default() -> Self {
        Self::new()
    }
}

impl QrLoginPublisher {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                scene_to_session: DashMap::new(),
                session_to_scene: DashMap::new(),
            }),
        }
    }

    /// 把 scene 绑定到指定 session。同一 session 已有 binding 时替换，旧 scene
    /// 静默断开（不主动推 `expired`，避免把客户端的二维码刷新动作误报成超时）。
    pub fn bind(&self, scene_id: String, session_id: SessionId) {
        if let Some((_, prev_scene)) = self.inner.session_to_scene.remove(&session_id) {
            self.inner.scene_to_session.remove(&prev_scene);
        }
        self.inner
            .scene_to_session
            .insert(scene_id.clone(), session_id.clone());
        self.inner.session_to_scene.insert(session_id, scene_id);
    }

    /// 按 `scene_id` 解绑（authorized / rejected / expired 推送后调用）。
    pub fn unbind_by_scene(&self, scene_id: &str) {
        if let Some((_, sid)) = self.inner.scene_to_session.remove(scene_id) {
            self.inner.session_to_scene.remove(&sid);
        }
    }

    /// 按 `session_id` 解绑（连接关闭时调用）。返回被解绑的 scene_id（如果有），
    /// 让调用方有机会通知 application 把 scene 标记为 aborted。
    pub fn unbind_by_session(&self, session_id: &SessionId) -> Option<String> {
        let (_, scene_id) = self.inner.session_to_scene.remove(session_id)?;
        self.inner.scene_to_session.remove(&scene_id);
        Some(scene_id)
    }

    /// 当前 binding 数量（监控 / 测试）。
    pub fn binding_count(&self) -> usize {
        self.inner.scene_to_session.len()
    }

    /// 查 binding 对应的 session（测试 / 调试用）。
    pub fn lookup_session(&self, scene_id: &str) -> Option<SessionId> {
        self.inner
            .scene_to_session
            .get(scene_id)
            .map(|v| v.value().clone())
    }

    /// 将事件推到 `scene_id` 对应的 unauth 连接。
    ///
    /// `terminal=true` 表示这是终态事件（authorized/rejected/expired），推完顺手清掉 binding。
    pub async fn push_event(
        &self,
        connection_manager: &ConnectionManager,
        event: QrLoginPushEvent,
        terminal: bool,
    ) -> PushOutcome {
        let scene_id = event.scene_id.clone();
        let session_id = match self.inner.scene_to_session.get(&scene_id) {
            Some(v) => v.value().clone(),
            None => {
                tracing::debug!(
                    "qr_login.publisher: no subscriber scene_id={} event={}",
                    scene_id,
                    event.event
                );
                return PushOutcome::NoSubscriber;
            }
        };

        let payload = match serde_json::to_vec(&event) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(
                    "qr_login.publisher: serialize event failed scene_id={} err={}",
                    scene_id,
                    e
                );
                if terminal {
                    self.unbind_by_scene(&scene_id);
                }
                return PushOutcome::NoSubscriber;
            }
        };

        let mut push_msg = PushMessageRequest::new();
        push_msg.topic = event.event.clone();
        push_msg.payload = payload;
        push_msg.timestamp = (chrono::Utc::now().timestamp_millis() / 1000) as u32;

        let sent = match connection_manager
            .send_unauth_event_to_session(session_id, &push_msg)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    "qr_login.publisher: transport send failed scene_id={} event={} err={}",
                    scene_id,
                    event.event,
                    e
                );
                if terminal {
                    self.unbind_by_scene(&scene_id);
                }
                return PushOutcome::NoSubscriber;
            }
        };

        if terminal {
            self.unbind_by_scene(&scene_id);
        }

        if sent > 0 {
            PushOutcome::Delivered
        } else {
            PushOutcome::NoSubscriber
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(n: u64) -> SessionId {
        SessionId::new(n)
    }

    /// #2 spec QR_API §5：同一 unauth session 重复 create_scene，旧 scene binding
    /// 必须被替换，并且双向 registry 都不残留旧条目。
    #[test]
    fn bind_replaces_existing_session_binding() {
        let pub_ = QrLoginPublisher::new();
        pub_.bind("scene_old".to_string(), sid(1));
        assert_eq!(pub_.binding_count(), 1);
        assert_eq!(pub_.lookup_session("scene_old"), Some(sid(1)));

        // 同一 session 重新绑定到新 scene
        pub_.bind("scene_new".to_string(), sid(1));

        assert_eq!(pub_.binding_count(), 1, "重复 bind 应替换不应累计");
        assert_eq!(
            pub_.lookup_session("scene_old"),
            None,
            "scene_to_session 残留旧 scene"
        );
        assert_eq!(
            pub_.lookup_session("scene_new"),
            Some(sid(1)),
            "新 scene 必须存在"
        );
        assert_eq!(
            pub_.unbind_by_session(&sid(1)),
            Some("scene_new".to_string()),
            "session_to_scene 必须只指向新 scene"
        );
    }

    /// 多个不同 session 各自绑定不同 scene，互不影响。
    #[test]
    fn bind_independent_sessions_coexist() {
        let pub_ = QrLoginPublisher::new();
        pub_.bind("scene_a".to_string(), sid(1));
        pub_.bind("scene_b".to_string(), sid(2));
        pub_.bind("scene_c".to_string(), sid(3));

        assert_eq!(pub_.binding_count(), 3);
        assert_eq!(pub_.lookup_session("scene_a"), Some(sid(1)));
        assert_eq!(pub_.lookup_session("scene_b"), Some(sid(2)));
        assert_eq!(pub_.lookup_session("scene_c"), Some(sid(3)));
    }

    /// `unbind_by_scene` 必须双向清理（authorized / rejected / expired 推送后）。
    #[test]
    fn unbind_by_scene_clears_both_directions() {
        let pub_ = QrLoginPublisher::new();
        pub_.bind("scene_x".to_string(), sid(42));

        pub_.unbind_by_scene("scene_x");

        assert_eq!(pub_.binding_count(), 0);
        assert_eq!(pub_.lookup_session("scene_x"), None);
        assert_eq!(
            pub_.unbind_by_session(&sid(42)),
            None,
            "session_to_scene 不能残留"
        );
    }

    /// #7 spec QR_API §5：连接关闭时 `unbind_by_session` 双向清理 +
    /// 返回原 scene_id（让调用方 logging）。
    #[test]
    fn unbind_by_session_clears_both_and_returns_scene() {
        let pub_ = QrLoginPublisher::new();
        pub_.bind("scene_y".to_string(), sid(99));

        let scene = pub_.unbind_by_session(&sid(99));

        assert_eq!(scene, Some("scene_y".to_string()));
        assert_eq!(pub_.binding_count(), 0);
        assert_eq!(
            pub_.lookup_session("scene_y"),
            None,
            "scene_to_session 不能残留"
        );
    }

    /// 没有 binding 时 `unbind_by_session` 返回 None，不报错。
    #[test]
    fn unbind_by_session_returns_none_when_unbound() {
        let pub_ = QrLoginPublisher::new();
        assert_eq!(pub_.unbind_by_session(&sid(7)), None);
    }

    /// 没有 binding 时 `unbind_by_scene` 是 no-op，不 panic。
    #[test]
    fn unbind_by_scene_unknown_is_noop() {
        let pub_ = QrLoginPublisher::new();
        pub_.unbind_by_scene("scene_does_not_exist");
        assert_eq!(pub_.binding_count(), 0);
    }

    /// #8 spec QR_API §5：未绑定时 `push_event` 返回 `NoSubscriber`，
    /// 不报错（让上层不要回滚 confirm）。
    #[tokio::test]
    async fn push_event_returns_no_subscriber_when_unbound() {
        let pub_ = QrLoginPublisher::new();
        let cm = crate::infra::ConnectionManager::new();

        let event = QrLoginPushEvent {
            event: "qr_login.authorized".to_string(),
            scene_id: "scene_missing".to_string(),
            state: "authorized".to_string(),
            data: None,
        };

        let outcome = pub_.push_event(&cm, event, true).await;
        assert_eq!(outcome, PushOutcome::NoSubscriber);
        assert_eq!(pub_.binding_count(), 0);
    }

    /// `push_event(terminal=true)` 即便 NoSubscriber 也是安全的：不会 panic、
    /// 不会留下半清理状态。
    #[tokio::test]
    async fn push_event_terminal_no_subscriber_idempotent() {
        let pub_ = QrLoginPublisher::new();
        let cm = crate::infra::ConnectionManager::new();
        let event = QrLoginPushEvent {
            event: "qr_login.expired".to_string(),
            scene_id: "ghost".to_string(),
            state: "expired".to_string(),
            data: None,
        };
        // 连续推两次相同事件，行为应一致。
        assert_eq!(
            pub_.push_event(&cm, event.clone(), true).await,
            PushOutcome::NoSubscriber
        );
        assert_eq!(
            pub_.push_event(&cm, event, true).await,
            PushOutcome::NoSubscriber
        );
    }
}
