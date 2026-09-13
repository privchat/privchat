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

use crate::domain::events::DomainEvent;
use crate::error::Result;
use crate::infra::connection_manager::ConnectionManager;
use crate::infra::event_bus::EventBus;
use crate::infra::redis::RedisClient;
use crate::push::intent_state::IntentStateManager;
use crate::push::types::{PushIntent, PushPayload};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

/// Push Planner（推送规划器）
///
/// 职责：
/// - 监听 DomainEvent::MessageCommitted 事件
/// - 检查用户是否在线（查询 Redis Presence）
/// - 如果离线，生成 PushIntent 并发送到 Worker
/// - 处理 MessageRevoked 和 UserOnline 事件（Phase 3）
/// 推送发出前的可取消窗口（毫秒）。
///
/// 这几秒是留给"真实送达"的：收件人在线时回执几百毫秒就回来，窗口内取消，他不会被通知；
/// 发送者几秒内撤回同样能把推送拦下来——APNs 一旦投递就再也收不回，只能在发之前拦。
///
/// 取值是个权衡：太短则回执来不及、在线用户会被通知打扰；太长则真正离线的人收到通知变慢。
/// 2 秒足够覆盖一次往返回执，而用户对"消息到手机"的感知阈值远在这之上。
const PUSH_CANCEL_WINDOW_MS: i64 = 2_000;

pub struct PushPlanner {
    redis: Option<Arc<RedisClient>>,
    connection_manager: Option<Arc<ConnectionManager>>,
    intent_state: Arc<IntentStateManager>, // Phase 3: 共享状态管理器
    /// 查会话免打扰用。None（单测/降级）时不做免打扰过滤。
    device_repo: Option<Arc<crate::repository::UserDeviceRepository>>,
}

impl PushPlanner {
    pub fn new() -> Self {
        Self {
            redis: None,
            connection_manager: None,
            intent_state: Arc::new(IntentStateManager::new()),
            device_repo: None,
        }
    }

    /// 创建带 Redis 的 Planner（Phase 2）
    pub fn with_redis(redis: Arc<RedisClient>) -> Self {
        Self {
            redis: Some(redis),
            connection_manager: None,
            intent_state: Arc::new(IntentStateManager::new()),
            device_repo: None,
        }
    }

    /// 创建带共享状态管理器的 Planner（Phase 3）
    pub fn with_state_manager(
        redis: Option<Arc<RedisClient>>,
        intent_state: Arc<IntentStateManager>,
    ) -> Self {
        Self {
            redis,
            connection_manager: None,
            intent_state,
            device_repo: None,
        }
    }

    /// 创建带共享状态管理器和连接真源的 Planner。
    pub fn with_state_manager_and_connection_manager(
        redis: Option<Arc<RedisClient>>,
        intent_state: Arc<IntentStateManager>,
        connection_manager: Arc<ConnectionManager>,
    ) -> Self {
        Self {
            redis,
            connection_manager: Some(connection_manager),
            intent_state,
            device_repo: None,
        }
    }

    /// 注入设备仓库（用于会话免打扰判定）。构造后链式调用。
    pub fn with_device_repo(
        mut self,
        device_repo: Arc<crate::repository::UserDeviceRepository>,
    ) -> Self {
        self.device_repo = Some(device_repo);
        self
    }

    /// 获取 Intent 状态管理器（供 Worker 使用）
    pub fn intent_state(&self) -> Arc<IntentStateManager> {
        Arc::clone(&self.intent_state)
    }

    /// 启动 Planner，监听事件
    pub async fn start(
        &self,
        event_bus: Arc<EventBus>,
        sender: tokio::sync::mpsc::Sender<PushIntent>,
    ) -> Result<()> {
        let mut receiver = event_bus.subscribe();
        let lagged_counter = event_bus.lagged_counter();

        info!("[PUSH PLANNER] Started");

        loop {
            match receiver.recv().await {
                Ok(event @ DomainEvent::MessageCommitted { .. }) => {
                    if let Err(e) = self.handle_message_committed(event, &sender).await {
                        error!("[PUSH PLANNER] Failed to handle MessageCommitted: {}", e);
                    }
                }
                Ok(event @ DomainEvent::MessageRevoked { .. }) => {
                    if let Err(e) = self.handle_message_revoked(event, &sender).await {
                        error!("[PUSH PLANNER] Failed to handle MessageRevoked: {}", e);
                    }
                }
                Ok(event @ DomainEvent::UserOnline { .. }) => {
                    if let Err(e) = self.handle_user_online(event).await {
                        error!("[PUSH PLANNER] Failed to handle UserOnline: {}", e);
                    }
                }
                Ok(event @ DomainEvent::MessageDelivered { .. }) => {
                    if let Err(e) = self.handle_message_delivered(event).await {
                        error!("[PUSH PLANNER] Failed to handle MessageDelivered: {}", e);
                    }
                }
                Ok(event @ DomainEvent::FriendRequestReceived { .. }) => {
                    if let Err(e) = self.handle_friend_request_received(event, &sender).await {
                        error!("[PUSH PLANNER] Failed to handle FriendRequestReceived: {}", e);
                    }
                }
                Ok(event @ DomainEvent::DeviceOnline { .. }) => {
                    if let Err(e) = self.handle_device_online(event).await {
                        error!("[PUSH PLANNER] Failed to handle DeviceOnline: {}", e);
                    }
                }
                Err(RecvError::Lagged(skipped)) => {
                    lagged_counter.fetch_add(1, Ordering::Relaxed);
                    warn!(
                        "⚠️ [PUSH PLANNER] EventBus lagged, skipped {} events (total_lagged={})",
                        skipped,
                        lagged_counter.load(Ordering::Relaxed)
                    );
                    // 不 sleep，立即继续接收后续事件
                }
                Err(RecvError::Closed) => {
                    error!("[PUSH PLANNER] EventBus closed, stopping");
                    break;
                }
            }
        }

        Ok(())
    }

    async fn handle_message_committed(
        &self,
        event: DomainEvent,
        sender: &tokio::sync::mpsc::Sender<PushIntent>,
    ) -> Result<()> {
        // 从 DomainEvent 中提取字段
        let (
            message_id,
            conversation_id,
            sender_id,
            recipient_id,
            content_preview,
            channel_type,
            message_type,
            timestamp,
            device_id,
        ) = match event {
            DomainEvent::MessageCommitted {
                message_id,
                conversation_id,
                sender_id,
                recipient_id,
                content_preview,
                channel_type,
                message_type,
                timestamp,
                device_id, // ✨ Phase 3.5: 可选的设备ID
                ..
            } => (
                message_id,
                conversation_id,
                sender_id,
                recipient_id,
                content_preview,
                channel_type,
                message_type,
                timestamp,
                device_id,
            ),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_committed");
                return Ok(());
            }
        };

        info!(
            "[PUSH PLANNER] Received MessageCommitted: message_id={}, recipient_id={}, sender_id={}, device_id={:?}",
            message_id, recipient_id, sender_id, device_id
        );

        // 收件人侧状态一次取齐：免打扰、未读总数、推送偏好。放在在线判定之前，
        // 因为这些判断与设备无关，两条 intent 路径（设备级 / 用户级）都要走。
        let ctx = match &self.device_repo {
            Some(repo) => match repo.load_push_context(recipient_id, conversation_id).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    // 读不到收件人的免打扰/隐私设置就**不推**。按默认值推的话，
                    // 数据库抖一下就会把正文推到一个关掉了预览的用户的锁屏上，
                    // 或者吵醒一个开了全局免打扰的用户——两件事都不可撤销，
                    // 而少推一条消息是可恢复的（用户打开 App 就看到了）。
                    error!(
                        "[PUSH PLANNER] 读取 user {} 的推送上下文失败，放弃本次推送: {}",
                        recipient_id, e
                    );
                    return Ok(());
                }
            },
            // repo 未注入只出现在单测里：生产装配一定带 repo（见 server.rs）。
            None => crate::repository::user_device_repo::PushContext::default(),
        };

        if ctx.global_mute {
            debug!(
                "[PUSH PLANNER] user {} 开启了全局免打扰，跳过推送",
                recipient_id
            );
            return Ok(());
        }
        if ctx.muted {
            debug!(
                "[PUSH PLANNER] 会话 {} 对 user {} 免打扰，跳过推送",
                conversation_id, recipient_id
            );
            return Ok(());
        }
        let unread_total = ctx.unread_total;

        // ✨ Phase 3.5: 如果指定了 device_id，只为该设备生成 Intent
        if let Some(device_id) = device_id {
            let intent_id = Uuid::new_v4().to_string();
            let intent = PushIntent::new(
                intent_id.clone(),
                message_id,
                conversation_id,
                recipient_id,
                device_id.clone(), // ✨ 设备级 Intent
                sender_id,
                PushPayload {
                    r#type: "new_message".to_string(),
                    conversation_id,
                    channel_type,
                    unread_total,
                    message_type: message_type.clone(),
                    show_preview: ctx.show_preview,
                    message_id,
                    sender_id,
                    content_preview,
                },
                timestamp,
                chrono::Utc::now().timestamp_millis() + PUSH_CANCEL_WINDOW_MS,
            );

            // 注册设备级 Intent
            self.intent_state
                .register_device_intent(&intent_id, message_id, recipient_id, &device_id)
                .await;

            // 发送到 Worker 队列
            sender.send(intent.clone()).await.map_err(|e| {
                crate::error::ServerError::Internal(format!(
                    "Failed to send intent to worker: {}",
                    e
                ))
            })?;

            debug!(
                "[PUSH PLANNER] Device-level Intent sent to worker: intent_id={}, device_id={}",
                intent.intent_id, device_id
            );
            return Ok(());
        }

        // 🔴 不再用"提交那一刻在不在线"决定推不推。
        //
        // 那个判断在 iOS 上是错的：App 被挂起之后 socket 还挂在 ConnectionManager 里，
        // 看起来在线，实际没人消费——直投 5 秒超时，而推送早就被跳过了，用户什么都收不到。
        // 实测按下 Home 之后的几十秒内消息既不送达也不通知。
        //
        // 改成：一律先排一条延迟推送，谁真的收到了谁来取消（送达回执 / 设备上线 / 撤回）。
        // 在线用户的回执几百毫秒就回来了，PUSH_CANCEL_WINDOW 内取消，他不会收到通知；
        // 真没收到的，窗口一过就推出去。判据从"我猜你在线"变成"你确实收到了"。
        debug!(
            "[PUSH PLANNER] Scheduling delayed push intent for user {} (cancellable within {}ms)",
            recipient_id, PUSH_CANCEL_WINDOW_MS
        );
        let not_before_ms = chrono::Utc::now().timestamp_millis() + PUSH_CANCEL_WINDOW_MS;

        // 生成用户级 PushIntent（兼容旧逻辑）
        let intent_id = Uuid::new_v4().to_string();
        let intent = PushIntent::new(
            intent_id.clone(),
            message_id,
            conversation_id,
            recipient_id,
            "".to_string(), // 旧逻辑：device_id 为空
            sender_id,
            PushPayload {
                r#type: "new_message".to_string(),
                conversation_id,
                channel_type,
                unread_total,
                message_type: message_type.clone(),
                show_preview: ctx.show_preview,
                message_id,
                sender_id,
                content_preview,
            },
            timestamp,
            not_before_ms,
        );

        // 注册 Intent（兼容旧逻辑）
        self.intent_state
            .register_intent(&intent_id, message_id, recipient_id)
            .await;

        // 发送到 Worker 队列
        sender.send(intent.clone()).await.map_err(|e| {
            crate::error::ServerError::Internal(format!("Failed to send intent to worker: {}", e))
        })?;

        debug!(
            "[PUSH PLANNER] Intent sent to worker: intent_id={}",
            intent.intent_id
        );

        Ok(())
    }

    /// 检查用户是否在线。
    ///
    /// 在线态真源是 ConnectionManager；这里禁止 Redis KEYS 热路径。
    async fn check_user_online(&self, user_id: u64) -> Result<bool> {
        if let Some(connection_manager) = &self.connection_manager {
            let connections = connection_manager.get_user_connections(user_id).await;
            if !connections.is_empty() {
                debug!(
                    "[PUSH PLANNER] User {} has {} online connection(s)",
                    user_id,
                    connections.len()
                );
                return Ok(true);
            }
            return Ok(false);
        }

        if self.redis.is_some() {
            debug!(
                "[PUSH PLANNER] Redis presence fallback is disabled to avoid KEYS; assuming user {} offline",
                user_id
            );
        } else {
            warn!(
                "[PUSH PLANNER] ConnectionManager not configured, assuming user {} offline",
                user_id
            );
        }
        Ok(false)
    }

    /// 处理消息撤销事件（Phase 3）
    /// 好友申请 → 远程推送。
    ///
    /// 好友申请此前只有 `connection_manager.send_push_to_user` 那一下 socket 广播：
    /// 对端没有活跃 session 时它直接返回空报告，没有离线队列也没有 APNs/FCM 兜底。
    /// 结果是 App 被杀掉时收到的好友申请毫无动静，只能等下次打开才发现。
    ///
    /// 与消息推送的差别只有两处：没有会话（conversation_id / message_id 记 0），
    /// 以及正文用申请人的名字而不是消息预览。延迟窗口和取消机制照旧——用户要是
    /// 真在线，socket 那条已经送到，`UserOnline` 会在窗口内把这条 intent 取消掉。
    async fn handle_friend_request_received(
        &self,
        event: DomainEvent,
        sender: &tokio::sync::mpsc::Sender<PushIntent>,
    ) -> Result<()> {
        let (requester_id, requester_name, target_user_id, timestamp) = match event {
            DomainEvent::FriendRequestReceived {
                requester_id,
                requester_name,
                target_user_id,
                timestamp,
            } => (requester_id, requester_name, target_user_id, timestamp),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_friend_request_received");
                return Ok(());
            }
        };

        // 与消息路径同一条原则：读不到收件人的免打扰/预览设置就不推。
        // 按默认值推会把申请人的名字推到一个关掉了预览的用户的锁屏上，而那是不可撤销的。
        let ctx = match &self.device_repo {
            Some(repo) => match repo.load_push_context(target_user_id, 0).await {
                Ok(ctx) => ctx,
                Err(e) => {
                    error!(
                        "[PUSH PLANNER] 读取 user {} 的推送上下文失败，放弃好友申请推送: {}",
                        target_user_id, e
                    );
                    return Ok(());
                }
            },
            None => crate::repository::user_device_repo::PushContext::default(),
        };

        if ctx.global_mute {
            debug!(
                "[PUSH PLANNER] user {} 开启了全局免打扰，跳过好友申请推送",
                target_user_id
            );
            return Ok(());
        }

        let not_before_ms = chrono::Utc::now().timestamp_millis() + PUSH_CANCEL_WINDOW_MS;
        let intent_id = Uuid::new_v4().to_string();
        let intent = PushIntent::new(
            intent_id.clone(),
            0,
            0,
            target_user_id,
            String::new(),
            requester_id,
            PushPayload {
                r#type: PushPayload::TYPE_FRIEND_REQUEST.to_string(),
                conversation_id: 0,
                channel_type: 0,
                unread_total: ctx.unread_total,
                message_type: String::new(),
                show_preview: ctx.show_preview,
                message_id: 0,
                sender_id: requester_id,
                // 好友申请没有正文，这里放申请人的名字供通知渲染。
                content_preview: requester_name,
            },
            timestamp,
            not_before_ms,
        );

        self.intent_state
            .register_intent(&intent_id, 0, target_user_id)
            .await;

        sender.send(intent).await.map_err(|e| {
            crate::error::ServerError::Internal(format!(
                "Failed to send friend request intent to worker: {}",
                e
            ))
        })?;
        info!(
            "[PUSH PLANNER] 好友申请推送已排队: requester={} target={}",
            requester_id, target_user_id
        );
        Ok(())
    }

    async fn handle_message_revoked(
        &self,
        event: DomainEvent,
        sender: &tokio::sync::mpsc::Sender<PushIntent>,
    ) -> Result<()> {
        let (message_id, conversation_id) = match event {
            DomainEvent::MessageRevoked {
                message_id,
                conversation_id,
                ..
            } => (message_id, conversation_id),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_revoked");
                return Ok(());
            }
        };
        // 谁曾经为这条消息排过推送，就给谁补一条"删通知"的静默推送。
        // 必须在 mark_revoked 之前取：那之后状态就不是 Pending 了，但名单还在。
        let notified_users = self
            .intent_state
            .users_with_intents_for_message(message_id)
            .await;

        info!(
            "[PUSH PLANNER] Received MessageRevoked: message_id={}",
            message_id
        );

        // 标记 Intent 为 revoked（Phase 3.5: 返回取消的数量）
        let count = self.intent_state.mark_revoked(message_id).await;
        // 🔴 标记只能拦住"还没发出去"的那些。
        //
        // 已经投递到设备上的通知，APNs 没有任何接口能收回——只能再发一条静默推送，
        // 请客户端自己把那条删掉。所以这里对所有曾经排过推送的用户都补一条：
        // 没发出去的那份刚被标成 revoked，多发一条静默推送客户端找不到对应通知，
        // 是个无害的空操作；漏发才是有代价的（撤回了，通知还挂在别人锁屏上）。
        for user_id in notified_users {
            let intent_id = Uuid::new_v4().to_string();
            let intent = PushIntent::new(
                intent_id,
                message_id,
                conversation_id,
                user_id,
                String::new(),
                0,
                PushPayload {
                    r#type: "revoke".to_string(),
                    conversation_id,
                    channel_type: 0,
                    unread_total: 0,
                    message_type: String::new(),
                    show_preview: false,
                    message_id,
                    sender_id: 0,
                    content_preview: String::new(),
                },
                chrono::Utc::now().timestamp(),
                // 立刻发：这条是去删通知的，晚一秒用户就多看一秒不该看到的内容。
                chrono::Utc::now().timestamp_millis(),
            );
            if let Err(e) = sender.send(intent).await {
                warn!(
                    "[PUSH PLANNER] 撤回静默推送入队失败 user={}, message={}: {}",
                    user_id, message_id, e
                );
            } else {
                info!(
                    "[PUSH PLANNER] Revoke silent push queued: user={}, message={}",
                    user_id, message_id
                );
            }
        }

        if count > 0 {
            info!(
                "[PUSH PLANNER] {} intent(s) marked as revoked for message {}",
                count, message_id
            );
        } else {
            debug!("[PUSH PLANNER] No intent found for message {}", message_id);
        }

        Ok(())
    }

    /// 处理用户上线事件（Phase 3，兼容旧逻辑）
    async fn handle_user_online(&self, event: DomainEvent) -> Result<()> {
        let user_id = match event {
            DomainEvent::UserOnline { user_id, .. } => user_id,
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_user_online");
                return Ok(());
            }
        };

        info!("[PUSH PLANNER] Received UserOnline: user_id={}", user_id);

        // 标记用户的所有待推送 Intent 为 cancelled
        let count = self.intent_state.mark_cancelled(user_id).await;
        if count > 0 {
            info!(
                "[PUSH PLANNER] {} intent(s) marked as cancelled for user {}",
                count, user_id
            );
        }

        Ok(())
    }

    /// ✨ Phase 3.5: 处理消息已送达事件
    async fn handle_message_delivered(&self, event: DomainEvent) -> Result<()> {
        let (message_id, user_id, device_id) = match event {
            DomainEvent::MessageDelivered {
                message_id,
                user_id,
                device_id,
                ..
            } => (message_id, user_id, device_id),
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_message_delivered");
                return Ok(());
            }
        };

        info!(
            "[PUSH PLANNER] Received MessageDelivered: message_id={}, user_id={}, device_id={}",
            message_id, user_id, device_id
        );

        // 取消这条消息对该用户的推送。
        //
        // device_id 为空 = 用户级送达回执（长连接投递成功），按「消息 + 用户」取消；
        // 非空则是设备级，按设备取消。
        let count = if device_id.is_empty() {
            self.intent_state
                .mark_cancelled_by_message_user(message_id, user_id)
                .await
        } else {
            self.intent_state
                .mark_cancelled_by_device(&device_id, Some(message_id))
                .await
        };
        if count == 0 {
            // 只在"这条消息确实有 intent、却没有一条属于该用户"时才出声。
            //
            // 发送者自己的那份投递也会走到这里（他的其它设备要收到回显），而发送者
            // 从来没有 intent——无条件打日志的话，每条消息都会报一次"未取消"。
            let known = self.intent_state.debug_intents_for_message(message_id).await;
            if !known.is_empty() {
                debug!(
                    "[PUSH PLANNER] MessageDelivered 未取消任何 intent: message_id={}, user_id={}, intents_for_message={:?}",
                    message_id, user_id, known,
                );
            }
        }
        if count > 0 {
            info!(
                "[PUSH PLANNER] {} intent(s) cancelled for device {} (message {})",
                count, device_id, message_id
            );
        }

        Ok(())
    }

    /// ✨ Phase 3.5: 处理设备上线事件
    async fn handle_device_online(&self, event: DomainEvent) -> Result<()> {
        let device_id = match event {
            DomainEvent::DeviceOnline { device_id, .. } => device_id,
            _ => {
                error!("[PUSH PLANNER] Unexpected event type in handle_device_online");
                return Ok(());
            }
        };

        info!(
            "[PUSH PLANNER] Received DeviceOnline: device_id={}",
            device_id
        );

        // 取消该设备的所有待推送 Intent
        let count = self
            .intent_state
            .mark_cancelled_by_device(&device_id, None)
            .await;
        if count > 0 {
            info!(
                "[PUSH PLANNER] {} intent(s) cancelled for device {}",
                count, device_id
            );
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::ConnectionManager;

    fn planner_with_connection_manager(connection_manager: Arc<ConnectionManager>) -> PushPlanner {
        PushPlanner::with_state_manager_and_connection_manager(
            None,
            Arc::new(IntentStateManager::new()),
            connection_manager,
        )
    }

    #[tokio::test]
    async fn check_user_online_uses_connection_manager() {
        let connection_manager = Arc::new(ConnectionManager::new());
        let session_id = msgtrans::SessionId::from(1_u64);
        connection_manager.register_connecting(session_id);
        connection_manager.authenticate(session_id, 42, "ios-42".to_string());

        let planner = planner_with_connection_manager(connection_manager);

        assert!(planner.check_user_online(42).await.unwrap());
    }

    #[tokio::test]
    async fn check_user_online_returns_false_without_connections() {
        let planner = planner_with_connection_manager(Arc::new(ConnectionManager::new()));

        assert!(!planner.check_user_online(42).await.unwrap());
    }
}
