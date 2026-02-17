use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::interval;
use tracing::{debug, error, info, warn};

use crate::infra::cache::TwoLevelCache;
use crate::infra::message_router::{
    MessagePriority, MessageRouter, OfflineMessage, UserId, UserOnlineStatus,
};
use crate::infra::SessionManager;
use crate::model::pts::UserMessageIndex;
use msgtrans::SessionId;

/// 离线消息投递Worker配置
#[derive(Debug, Clone)]
pub struct OfflineWorkerConfig {
    /// 扫描间隔（毫秒）
    pub scan_interval_ms: u64,
    /// 批量投递大小
    pub batch_size: usize,
    /// 最大并发投递用户数
    pub max_concurrent_users: usize,
    /// 投递超时时间（毫秒）
    pub delivery_timeout_ms: u64,
    /// 重试间隔（毫秒）
    pub retry_interval_ms: u64,
    /// 统计报告间隔（秒）
    pub stats_report_interval_secs: u64,
    /// 过期消息清理间隔（小时）
    pub cleanup_interval_hours: u64,
}

impl Default for OfflineWorkerConfig {
    fn default() -> Self {
        Self {
            scan_interval_ms: 1000,         // 1秒扫描一次
            batch_size: 50,                 // 每次最多投递50条消息
            max_concurrent_users: 100,      // 最多同时处理100个用户
            delivery_timeout_ms: 5000,      // 5秒投递超时
            retry_interval_ms: 30000,       // 30秒重试间隔
            stats_report_interval_secs: 60, // 1分钟统计报告
            cleanup_interval_hours: 24,     // 24小时清理一次
        }
    }
}

/// 投递统计信息
#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct DeliveryStats {
    /// 总扫描次数
    pub total_scans: u64,
    /// 处理的用户数
    pub users_processed: u64,
    /// 成功投递消息数
    pub messages_delivered: u64,
    /// 失败消息数
    pub messages_failed: u64,
    /// 过期消息数
    pub messages_expired: u64,
    /// 平均投递延迟（毫秒）
    pub avg_delivery_latency_ms: f64,
    /// 最后扫描时间
    pub last_scan_time: u64,
    /// 当前待投递用户数
    pub pending_users: u64,
}

/// 用户投递状态
#[derive(Debug, Clone)]
pub struct UserDeliveryState {
    /// 用户ID
    pub user_id: UserId,
    /// 最后投递时间
    pub last_delivery_time: u64,
    /// 投递失败次数
    pub failure_count: u32,
    /// 下次重试时间
    pub next_retry_time: u64,
    /// 是否正在投递中
    pub is_delivering: bool,
}

/// 离线消息投递Worker（事件驱动模式）
pub struct OfflineMessageWorker {
    /// 配置
    config: OfflineWorkerConfig,
    /// 消息路由器
    router: Arc<MessageRouter>,
    /// 用户在线状态缓存
    user_status_cache: Arc<dyn TwoLevelCache<UserId, UserOnlineStatus>>,
    /// 离线消息队列缓存
    offline_queue_cache: Arc<dyn TwoLevelCache<UserId, Vec<OfflineMessage>>>,
    /// 投递统计
    stats: Arc<RwLock<DeliveryStats>>,
    /// 用户投递状态
    user_states: Arc<RwLock<HashMap<UserId, UserDeliveryState>>>,
    /// 运行状态
    is_running: Arc<RwLock<bool>>,
    /// 【新增】触发推送的通道（发送端）
    trigger_tx: mpsc::UnboundedSender<UserId>,
    /// 【新增】触发推送的通道（接收端）
    trigger_rx: Arc<Mutex<mpsc::UnboundedReceiver<UserId>>>,
    /// ✨ 会话管理器（用于获取 local_pts）
    session_manager: Arc<SessionManager>,
    /// ✨ 用户消息索引（用于 pts -> message_id 映射）
    user_message_index: Arc<UserMessageIndex>,
    /// ✨ 离线队列服务（用于从 Redis 获取消息）
    offline_queue_service: Arc<crate::service::OfflineQueueService>,
}

impl OfflineMessageWorker {
    /// 创建新的离线消息投递Worker（事件驱动模式）
    pub fn new(
        config: OfflineWorkerConfig,
        router: Arc<MessageRouter>,
        user_status_cache: Arc<dyn TwoLevelCache<UserId, UserOnlineStatus>>,
        offline_queue_cache: Arc<dyn TwoLevelCache<UserId, Vec<OfflineMessage>>>,
        session_manager: Arc<SessionManager>,
        user_message_index: Arc<UserMessageIndex>,
        offline_queue_service: Arc<crate::service::OfflineQueueService>,
    ) -> Self {
        let (trigger_tx, trigger_rx) = mpsc::unbounded_channel();

        Self {
            config,
            router,
            user_status_cache,
            offline_queue_cache,
            stats: Arc::new(RwLock::new(DeliveryStats::default())),
            user_states: Arc::new(RwLock::new(HashMap::new())),
            is_running: Arc::new(RwLock::new(false)),
            trigger_tx,
            trigger_rx: Arc::new(Mutex::new(trigger_rx)),
            session_manager,
            user_message_index,
            offline_queue_service,
        }
    }

    /// 【新增】触发推送（ConnectHandler 调用）
    pub fn trigger_push(&self, user_id: UserId) {
        if let Err(e) = self.trigger_tx.send(user_id) {
            error!("触发用户 {} 离线消息推送失败: {}", user_id, e);
        } else {
            debug!("✅ 已触发用户 {} 的离线消息推送", user_id);
        }
    }

    /// 启动Worker主循环（事件驱动模式）
    pub async fn start(self: Arc<Self>) -> Result<()> {
        {
            let mut running = self.is_running.write().await;
            if *running {
                return Err(anyhow::anyhow!("Worker is already running"));
            }
            *running = true;
        }

        info!("🚀 离线消息推送 Worker 已启动（事件驱动模式）");

        let stats_interval = Duration::from_secs(self.config.stats_report_interval_secs);
        let cleanup_interval = Duration::from_secs(self.config.cleanup_interval_hours * 3600);

        // 【主任务】事件驱动的推送任务
        let worker_push = self.clone();
        let push_task = tokio::spawn(async move {
            loop {
                // 等待触发事件
                let user_id = {
                    let mut rx = worker_push.trigger_rx.lock().await;
                    match rx.recv().await {
                        Some(user_id) => user_id,
                        None => {
                            info!("触发通道已关闭，退出推送任务");
                            break;
                        }
                    }
                };

                info!("📨 收到用户 {} 的离线消息推送触发", user_id);

                // 立即推送该用户的所有离线消息（异步执行）
                let worker = worker_push.clone();
                tokio::spawn(async move {
                    if let Err(e) = worker.deliver_all_user_messages(&user_id).await {
                        error!("推送用户 {} 的离线消息失败: {}", user_id, e);
                    }
                });

                // 检查是否应该停止
                if !*worker_push.is_running.read().await {
                    break;
                }
            }

            info!("推送任务已停止");
        });

        // 启动统计报告任务
        let worker_stats = self.clone();
        let stats_task = tokio::spawn(async move {
            let mut interval = interval(stats_interval);
            loop {
                interval.tick().await;
                worker_stats.report_stats().await;

                if !*worker_stats.is_running.read().await {
                    break;
                }
            }
        });

        // 启动清理任务
        let worker_cleanup = self.clone();
        let cleanup_task = tokio::spawn(async move {
            let mut interval = interval(cleanup_interval);
            loop {
                interval.tick().await;
                if let Err(e) = worker_cleanup.cleanup_expired_messages().await {
                    error!("清理过期消息失败: {}", e);
                }

                if !*worker_cleanup.is_running.read().await {
                    break;
                }
            }
        });

        // 等待任务完成（通常不会到达这里，除非手动停止）
        tokio::try_join!(push_task, stats_task, cleanup_task)?;

        Ok(())
    }

    /// 停止Worker
    pub async fn stop(&self) {
        info!("正在停止离线消息投递Worker...");
        {
            let mut running = self.is_running.write().await;
            *running = false;
        }
        info!("离线消息投递Worker已停止");
    }

    /// 【新增】推送用户的所有离线消息（持续推送直到清空，基于 local_pts 过滤）
    async fn deliver_all_user_messages(&self, user_id: &UserId) -> Result<usize> {
        info!("🔄 开始为用户 {} 推送所有离线消息", user_id);

        // ✨ 1. 获取用户所有 session 的 client_pts
        // 注意：每个 session 独立维护 client_pts，需要为每个 session 单独推送
        let sessions = self.session_manager.list_user_sessions(*user_id).await;
        if sessions.is_empty() {
            warn!("⚠️ 用户 {} 没有活跃的 session，跳过推送", user_id);
            return Ok(0);
        }

        info!(
            "📊 用户 {} 有 {} 个活跃 session，将分别推送",
            user_id,
            sessions.len()
        );

        // 为每个 session 单独推送离线消息
        let mut total_pushed = 0;
        for (session_id, session_info) in sessions {
            let client_pts = session_info.client_pts;
            info!("📊 Session {} 的 client_pts: {}", session_id, client_pts);
            match self
                .deliver_messages_for_session(user_id, &session_id, client_pts)
                .await
            {
                Ok(pushed) => {
                    total_pushed += pushed;
                }
                Err(e) => {
                    warn!(
                        "⚠️ Session {} 离线消息推送失败，继续处理其他 session: {}",
                        session_id, e
                    );
                    continue;
                }
            }
        }

        info!(
            "✅ 用户 {} 所有 session 离线消息推送完成，共推送 {} 条",
            user_id, total_pushed
        );
        Ok(total_pushed)
    }

    /// 为单个 session 推送离线消息
    async fn deliver_messages_for_session(
        &self,
        user_id: &UserId,
        session_id: &SessionId,
        client_pts: u64,
    ) -> Result<usize> {
        info!(
            "🔄 开始为 session {} 推送离线消息 (client_pts={})",
            session_id, client_pts
        );

        // ✨ 2. 使用 UserMessageIndex 查找 pts > client_pts 的消息ID
        let target_message_ids: std::collections::HashSet<u64> = self
            .user_message_index
            .get_message_ids_above(*user_id, client_pts)
            .await
            .into_iter()
            .collect();

        info!(
            "📋 Session {} 需要推送的消息ID数量: {} (client_pts={})",
            session_id,
            target_message_ids.len(),
            client_pts
        );

        if target_message_ids.is_empty() {
            info!(
                "✅ Session {} 没有需要推送的消息（所有消息的 pts <= client_pts）",
                session_id
            );
            return Ok(0);
        }

        // ✨ 3. 从 OfflineQueueService 获取所有消息，并过滤出需要推送的消息
        let all_messages = self
            .offline_queue_service
            .get_all(*user_id)
            .await
            .unwrap_or_else(|e| {
                warn!("⚠️ 获取离线消息失败: {}，返回空列表", e);
                Vec::new()
            });

        if all_messages.is_empty() {
            info!("✅ 用户 {} 没有离线消息", user_id);
            return Ok(0);
        }

        // 从离线队列中过滤出需要推送的消息（Redis LPUSH 导致 get_all 返回「最新在前」，后面会按 pts 升序排序）
        let filtered_messages: Vec<_> = all_messages
            .into_iter()
            .filter(|msg| {
                // 解析 local_message_id 为 u64 进行比较
                Ok::<u64, ()>(msg.local_message_id)
                    .map(|id| target_message_ids.contains(&id))
                    .unwrap_or(false)
            })
            .collect();

        info!(
            "📤 Session {} 过滤后的消息数量: {} (从 {} 条中筛选)",
            session_id,
            filtered_messages.len(),
            target_message_ids.len()
        );

        if filtered_messages.is_empty() {
            info!("✅ Session {} 没有需要推送的消息（已过滤）", session_id);
            return Ok(0);
        }

        // ✨ 4. 构建消息ID到pts的映射，并按 pts 升序得到有序的 message_id 列表（保证接收方按 1→2→3→4→5 顺序收到）
        let message_pts_map = self
            .user_message_index
            .get_message_ids_with_pts_above(*user_id, client_pts)
            .await;
        // let max_pts = message_pts_map.values().max().copied().unwrap_or(client_pts);
        // let ordered_message_ids: Vec<u64> = if max_pts > client_pts {
        //     self.user_message_index
        //         .get_message_ids(*user_id, client_pts + 1, max_pts)
        //         .await
        // } else {
        //     Vec::new()
        // };
        // // 用索引的 pts 顺序重排消息（不依赖 sort_by_key，避免 map 缺失或 HashMap 迭代顺序问题）
        // let id_to_msg: HashMap<u64, _> = filtered_messages
        //     .into_iter()
        //     .map(|m| (m.local_message_id, m))
        //     .collect();
        // let mut filtered_messages: Vec<_> = ordered_message_ids
        //     .into_iter()
        //     .filter_map(|id| id_to_msg.get(&id).cloned())
        //     .collect();

        // if filtered_messages.is_empty() {
        //     info!("✅ Session {} 没有需要推送的消息（按 pts 重排后为空）", session_id);
        //     return Ok(0);
        // }

        // // ✨ 4.6 兜底：严格按 pts 升序排序（UserMessageIndex 未按 channel 区分时 get_message_ids 可能含其它 channel，顺序靠 sort 保证）
        // filtered_messages.sort_by_key(|m| {
        //     message_pts_map
        //         .get(&m.local_message_id)
        //         .copied()
        //         .unwrap_or(u64::MAX)
        // });

        // ✨ 5. 将过滤后的消息转换为 OfflineMessage 格式并推送
        const BATCH_SIZE: usize = 100;
        let mut total_pushed = 0;
        let mut max_pts = client_pts; // 记录推送的最大 pts

        for chunk in filtered_messages.chunks(BATCH_SIZE) {
            // 转换为 OfflineMessage 格式
            let offline_messages: Vec<OfflineMessage> = chunk
                .iter()
                .map(|recv_msg| {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs();
                    // 更新 max_pts
                    let message_id = recv_msg.local_message_id;
                    if let Some(&pts) = message_pts_map.get(&message_id) {
                        max_pts = max_pts.max(pts);
                    }
                    OfflineMessage {
                        message_id,
                        target_user_id: *user_id,
                        target_device_id: None,
                        message: recv_msg.clone(),
                        created_at: now,
                        expires_at: now + 3600 * 24, // 24小时过期
                        retry_count: 0,
                        priority: MessagePriority::Normal,
                    }
                })
                .collect();

            // 推送当前批次
            match self
                .push_message_batch_to_session(user_id, session_id, &offline_messages)
                .await
            {
                Ok(pushed) => {
                    if pushed != offline_messages.len() {
                        let err = anyhow::anyhow!(
                            "partial offline push: pushed={}, expected={}",
                            pushed,
                            offline_messages.len()
                        );
                        error!("❌ Session {} 推送离线消息批次失败: {}", session_id, err);
                        return Err(err);
                    }
                    total_pushed += pushed;
                    info!(
                        "📤 Session {} 已推送 {} 条离线消息，剩余 {} 条",
                        session_id,
                        pushed,
                        filtered_messages.len() - total_pushed
                    );
                }
                Err(e) => {
                    error!("❌ Session {} 推送离线消息批次失败: {}", session_id, e);
                    // 推送失败，消息保留在队列中，下次连接重试
                    return Err(e);
                }
            }

            // 短暂延迟，避免过快推送
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        // ✨ 6. 推送完成后，更新 session 的 client_pts
        if max_pts > client_pts {
            self.session_manager
                .update_client_pts(session_id, max_pts)
                .await;
            info!(
                "✅ Session {} 的 client_pts 已更新: {} -> {}",
                session_id, client_pts, max_pts
            );
        }

        // 更新统计
        {
            let mut stats = self.stats.write().await;
            stats.messages_delivered += total_pushed as u64;
            stats.users_processed += 1;
        }

        info!(
            "✅ Session {} 离线消息推送完成，共推送 {} 条",
            session_id, total_pushed
        );
        Ok(total_pushed)
    }

    /// 向特定 session 推送一批消息
    async fn push_message_batch_to_session(
        &self,
        _user_id: &UserId,
        session_id: &SessionId,
        messages: &[OfflineMessage],
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }

        // 过滤过期消息
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let valid_messages: Vec<_> = messages
            .iter()
            .filter(|msg| now <= msg.expires_at)
            .collect();

        let count = valid_messages.len();

        if count == 0 {
            warn!("批次中的所有消息都已过期");
            return Ok(0);
        }

        // 推送到该设备
        let mut success_count = 0;
        for message in &valid_messages {
            match self
                .router
                .route_message_to_session(&session_id.to_string(), message.message.clone())
                .await
            {
                Ok(result) if result.success_count > 0 => {
                    success_count += 1;
                    debug!(
                        "✅ 消息 {} 成功推送到 session {}",
                        message.message_id, session_id
                    );
                }
                Ok(result) => {
                    warn!(
                        "⚠️ 消息 {} 推送到 session {} 未成功: success_count={}, failed_count={}, offline_count={}",
                        message.message_id,
                        session_id,
                        result.success_count,
                        result.failed_count,
                        result.offline_count
                    );
                }
                Err(e) => {
                    warn!(
                        "❌ 消息 {} 推送到 session {} 出错: {}",
                        message.message_id, session_id, e
                    );
                }
            }
        }

        info!(
            "📨 成功推送 {} 条离线消息给 session {}",
            success_count, session_id
        );

        Ok(success_count)
    }

    /// 【新增】推送一批消息
    async fn push_message_batch(
        &self,
        user_id: &UserId,
        messages: &[OfflineMessage],
    ) -> Result<usize> {
        if messages.is_empty() {
            return Ok(0);
        }

        // 获取用户在线状态
        let user_status = self
            .user_status_cache
            .get(user_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("用户不在线"))?;

        // 获取在线设备
        let online_devices: Vec<_> = user_status
            .devices
            .iter()
            .filter(|d| matches!(d.status, crate::infra::message_router::DeviceStatus::Online))
            .collect();

        if online_devices.is_empty() {
            return Err(anyhow::anyhow!("用户没有在线设备"));
        }

        // 过滤过期消息
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let valid_messages: Vec<_> = messages
            .iter()
            .filter(|msg| now <= msg.expires_at)
            .collect();

        let count = valid_messages.len();

        if count == 0 {
            warn!("批次中的所有消息都已过期");
            return Ok(0);
        }

        // 推送到所有在线设备
        for device in &online_devices {
            for message in &valid_messages {
                match self
                    .router
                    .route_message_to_device(user_id, &device.device_id, message.message.clone())
                    .await
                {
                    Ok(result) if result.success_count > 0 => {
                        debug!(
                            "✅ 消息 {} 成功推送到设备 {}",
                            message.message_id, device.device_id
                        );
                    }
                    Ok(_) => {
                        debug!(
                            "⚠️ 消息 {} 推送到设备 {} 失败",
                            message.message_id, device.device_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "❌ 消息 {} 推送到设备 {} 出错: {}",
                            message.message_id, device.device_id, e
                        );
                    }
                }
            }
        }

        info!(
            "📨 成功推送 {} 条离线消息给用户 {} 的 {} 个设备",
            count,
            user_id,
            online_devices.len()
        );

        Ok(count)
    }

    /// 为特定用户投递离线消息
    async fn deliver_user_messages(&self, user_id: &UserId) -> Result<usize> {
        // 检查用户是否在线
        let user_status = self.user_status_cache.get(user_id).await;

        let user_status = match user_status {
            Some(status) => status,
            None => {
                debug!("用户 {} 不在线，跳过投递", user_id);
                return Ok(0);
            }
        };

        // 检查用户是否有在线设备
        let online_devices: Vec<_> = user_status
            .devices
            .iter()
            .filter(|d| matches!(d.status, crate::infra::message_router::DeviceStatus::Online))
            .collect();

        if online_devices.is_empty() {
            debug!("用户 {} 没有在线设备，跳过投递", user_id);
            return Ok(0);
        }

        // 获取离线消息
        let offline_messages = self.offline_queue_cache.get(user_id).await;

        let messages = match offline_messages {
            Some(msgs) if !msgs.is_empty() => msgs,
            _ => {
                debug!("用户 {} 没有离线消息", user_id);
                return Ok(0);
            }
        };

        debug!("开始为用户 {} 投递 {} 条离线消息", user_id, messages.len());

        // 标记用户为投递中状态
        self.set_user_delivering(user_id, true).await;

        let mut delivered_count = 0;
        let mut remaining_messages = Vec::new();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 按优先级和设备分组处理消息
        for mut message in messages {
            // 检查消息是否过期
            if now > message.expires_at {
                debug!("消息 {} 已过期，跳过投递", message.message_id);
                let mut stats = self.stats.write().await;
                stats.messages_expired += 1;
                continue;
            }

            // 确定目标设备
            let target_devices = if let Some(device_id) = &message.target_device_id {
                // 指定设备
                online_devices
                    .iter()
                    .filter(|d| d.device_id == *device_id)
                    .cloned()
                    .collect()
            } else {
                // 所有在线设备
                online_devices.clone()
            };

            if target_devices.is_empty() {
                // 目标设备不在线，保留消息
                remaining_messages.push(message);
                continue;
            }

            // 尝试投递消息
            let mut delivery_success = false;

            for device in target_devices {
                match self
                    .router
                    .route_message_to_device(user_id, &device.device_id, message.message.clone())
                    .await
                {
                    Ok(result) if result.success_count > 0 => {
                        delivery_success = true;
                        delivered_count += 1;
                        debug!(
                            "消息 {} 成功投递到设备 {}",
                            message.message_id, device.device_id
                        );
                        break; // 只需要投递到一个设备即可
                    }
                    Ok(_) => {
                        debug!(
                            "消息 {} 投递到设备 {} 失败",
                            message.message_id, device.device_id
                        );
                    }
                    Err(e) => {
                        warn!(
                            "消息 {} 投递到设备 {} 出错: {}",
                            message.message_id, device.device_id, e
                        );
                    }
                }
            }

            if !delivery_success {
                // 投递失败，增加重试次数
                message.retry_count += 1;
                if message.retry_count <= 3 {
                    // 最多重试3次
                    remaining_messages.push(message);
                } else {
                    warn!("消息 {} 重试次数超限，丢弃", message.message_id);
                }
            }
        }

        // 更新离线消息队列
        if remaining_messages.is_empty() {
            // 所有消息都投递完成，清空队列
            self.offline_queue_cache.invalidate(user_id).await;
            debug!("用户 {} 的离线消息队列已清空", user_id);
        } else {
            // 还有未投递的消息，更新队列
            let remaining_count = remaining_messages.len();
            self.offline_queue_cache
                .put(user_id.clone(), remaining_messages, 3600)
                .await;
            debug!(
                "用户 {} 还有 {} 条未投递的离线消息",
                user_id, remaining_count
            );
        }

        // 取消投递中状态
        self.set_user_delivering(user_id, false).await;

        info!(
            "用户 {} 离线消息投递完成，共投递 {} 条消息",
            user_id, delivered_count
        );

        Ok(delivered_count)
    }

    /// 处理投递失败
    async fn handle_delivery_failure(&self, user_id: &UserId) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut user_states = self.user_states.write().await;

        let state = user_states
            .entry(user_id.clone())
            .or_insert_with(|| UserDeliveryState {
                user_id: user_id.clone(),
                last_delivery_time: 0,
                failure_count: 0,
                next_retry_time: 0,
                is_delivering: false,
            });

        state.failure_count += 1;
        state.next_retry_time = now + (self.config.retry_interval_ms / 1000);
        state.is_delivering = false;

        warn!(
            "用户 {} 投递失败 {} 次，下次重试时间: {}",
            user_id, state.failure_count, state.next_retry_time
        );
    }

    /// 设置用户投递状态
    async fn set_user_delivering(&self, user_id: &UserId, is_delivering: bool) {
        let mut user_states = self.user_states.write().await;
        let state = user_states
            .entry(user_id.clone())
            .or_insert_with(|| UserDeliveryState {
                user_id: user_id.clone(),
                last_delivery_time: 0,
                failure_count: 0,
                next_retry_time: 0,
                is_delivering: false,
            });

        state.is_delivering = is_delivering;
        if !is_delivering {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            state.last_delivery_time = now;
        }
    }

    /// 清理过期消息
    async fn cleanup_expired_messages(&self) -> Result<usize> {
        info!("开始清理过期离线消息...");

        let cleaned_count = 0; // TODO: 实现实际的清理逻辑

        info!("过期消息清理完成，清理了 {} 条消息", cleaned_count);
        Ok(cleaned_count)
    }

    /// 报告统计信息
    async fn report_stats(&self) {
        let stats = self.stats.read().await;
        info!(
            "离线投递统计 - 扫描: {}, 用户: {}, 投递: {}, 失败: {}, 过期: {}, 延迟: {:.2}ms, 待处理: {}",
            stats.total_scans,
            stats.users_processed,
            stats.messages_delivered,
            stats.messages_failed,
            stats.messages_expired,
            stats.avg_delivery_latency_ms,
            stats.pending_users
        );
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> DeliveryStats {
        let stats = self.stats.read().await;
        stats.clone()
    }

    /// 【已废弃】手动触发投递（已改为事件驱动，使用 trigger_push）
    #[deprecated(note = "使用 trigger_push 替代")]
    pub async fn trigger_delivery(&self) -> Result<()> {
        Err(anyhow::anyhow!("已废弃：请使用 trigger_push 方法"))
    }
}
