use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::infra::cache::TwoLevelCache;
// 暂时注释掉 redis 相关导入
// use crate::infra::redis_cache::SerializedRedisCache;
use privchat_protocol::protocol::PushMessageRequest;

/// 用户ID类型
pub type UserId = u64;
/// 会话ID类型
pub type SessionId = String;
/// 消息ID类型
pub type MessageId = u64;
/// 设备ID类型
pub type DeviceId = String;  // 保留UUID字符串

/// 用户在线状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserOnlineStatus {
    /// 用户ID
    pub user_id: UserId,
    /// 在线设备列表
    pub devices: Vec<DeviceSession>,
    /// 最后活跃时间
    pub last_active: u64,
}

/// 设备会话信息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeviceSession {
    /// 设备ID
    pub device_id: DeviceId,
    /// 会话ID
    pub session_id: SessionId,
    /// 设备类型
    pub device_type: String,
    /// 上线时间
    pub online_time: u64,
    /// 最后活跃时间
    pub last_active: u64,
    /// 设备状态
    pub status: DeviceStatus,
}

/// 设备状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum DeviceStatus {
    #[default] Online,
    Away,
    Busy,
    Offline,
}

/// 离线消息
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OfflineMessage {
    /// 消息ID
    pub message_id: MessageId,
    /// 目标用户ID
    pub target_user_id: UserId,
    /// 目标设备ID（可选，如果指定则只发送给特定设备）
    pub target_device_id: Option<DeviceId>,
    /// 消息内容
    pub message: PushMessageRequest,
    /// 创建时间
    pub created_at: u64,
    /// 过期时间
    pub expires_at: u64,
    /// 重试次数
    pub retry_count: u32,
    /// 优先级
    pub priority: MessagePriority,
}

/// 消息优先级
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum MessagePriority {
    Low = 0,
    #[default] Normal = 1,
    High = 2,
    Urgent = 3,
}

/// 消息路由配置
#[derive(Debug, Clone)]
pub struct MessageRouterConfig {
    /// 离线消息最大保留时间（秒）
    pub offline_message_ttl: u64,
    /// 离线消息最大重试次数
    pub max_retry_count: u32,
    /// 批量推送最大消息数
    pub max_batch_size: usize,
    /// 消息路由超时时间（毫秒）
    pub route_timeout_ms: u64,
    /// 离线队列最大长度
    pub max_offline_queue_size: usize,
}

impl Default for MessageRouterConfig {
    fn default() -> Self {
        Self {
            offline_message_ttl: 7 * 24 * 3600, // 7天
            max_retry_count: 3,
            max_batch_size: 100,
            route_timeout_ms: 5000, // 5秒
            max_offline_queue_size: 10000,
        }
    }
}

/// 消息路由结果
#[derive(Debug, Clone)]
pub struct RouteResult {
    /// 成功路由的设备数量
    pub success_count: usize,
    /// 失败路由的设备数量
    pub failed_count: usize,
    /// 离线设备数量
    pub offline_count: usize,
    /// 路由延迟（毫秒）
    pub latency_ms: u64,
}

/// 消息路由器
pub struct MessageRouter {
    /// 配置
    config: MessageRouterConfig,
    /// 用户在线状态缓存：UserId -> UserOnlineStatus
    user_status_cache: Arc<dyn TwoLevelCache<UserId, UserOnlineStatus>>,
    /// 离线消息队列：UserId -> Vec<OfflineMessage>
    offline_queue: Arc<dyn TwoLevelCache<UserId, Vec<OfflineMessage>>>,
    /// 会话管理器，用于实际发送消息
    session_manager: Arc<dyn SessionManager>,
    /// 消息分发统计
    stats: Arc<RwLock<MessageRouterStats>>,
}

/// 消息路由统计
#[derive(Debug, Default)]
pub struct MessageRouterStats {
    /// 总消息数
    pub total_messages: u64,
    /// 成功路由数
    pub success_routes: u64,
    /// 失败路由数
    pub failed_routes: u64,
    /// 离线消息数
    pub offline_messages: u64,
    /// 平均延迟
    pub avg_latency_ms: f64,
}

/// 会话管理器trait - 用于实际发送消息到客户端
#[async_trait::async_trait]
pub trait SessionManager: Send + Sync {
    /// 向指定会话发送消息
    async fn send_to_session(&self, session_id: &SessionId, message: &PushMessageRequest) -> Result<()>;
    
    /// 批量向多个会话发送消息
    async fn send_to_sessions(&self, sessions: &[SessionId], message: &PushMessageRequest) -> Result<Vec<Result<()>>>;
    
    /// 批量向会话发送多条消息
    async fn send_batch_to_session(&self, session_id: &SessionId, messages: &[PushMessageRequest]) -> Result<()>;
    
    /// 检查会话是否在线
    async fn is_session_online(&self, session_id: &SessionId) -> bool;
    
    /// 获取在线会话列表
    async fn get_online_sessions(&self) -> Vec<SessionId>;
}

impl MessageRouter {
    /// 创建新的消息路由器
    pub fn new(
        config: MessageRouterConfig,
        user_status_cache: Arc<dyn TwoLevelCache<UserId, UserOnlineStatus>>,
        offline_queue: Arc<dyn TwoLevelCache<UserId, Vec<OfflineMessage>>>,
        session_manager: Arc<dyn SessionManager>,
    ) -> Self {
        Self {
            config,
            user_status_cache,
            offline_queue,
            session_manager,
            stats: Arc::new(RwLock::new(MessageRouterStats::default())),
        }
    }

    /// 路由消息到指定用户
    pub async fn route_message_to_user(
        &self,
        user_id: &UserId,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        
        // 检查用户在线状态
        let user_status = self.user_status_cache.get(user_id).await;
        
        match user_status {
            Some(status) => {
                // 用户在线，路由到所有在线设备
                self.route_to_online_devices(&status, message).await
            }
            None => {
                // 用户不在线，存储为离线消息
                self.store_offline_message(user_id, message).await?;
                
                let elapsed = start_time.elapsed().unwrap_or_default();
                Ok(RouteResult {
                    success_count: 0,
                    failed_count: 0,
                    offline_count: 1,
                    latency_ms: elapsed.as_millis() as u64,
                })
            }
        }
    }

    /// 路由消息到指定用户的特定设备
    pub async fn route_message_to_device(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        
        // 检查用户在线状态
        let user_status = self.user_status_cache.get(user_id).await;
        
        match user_status {
            Some(status) => {
                // 查找指定设备
                if let Some(device) = status.devices.iter().find(|d| d.device_id == *device_id) {
                    if matches!(device.status, DeviceStatus::Online) {
                        // 设备在线，直接发送
                        match self.session_manager.send_to_session(&device.session_id, &message).await {
                            Ok(_) => {
                                let elapsed = start_time.elapsed().unwrap_or_default();
                                Ok(RouteResult {
                                    success_count: 1,
                                    failed_count: 0,
                                    offline_count: 0,
                                    latency_ms: elapsed.as_millis() as u64,
                                })
                            }
                            Err(_) => {
                                // 发送失败，存储为离线消息
                                self.store_offline_message_with_device(user_id, device_id, message).await?;
                                let elapsed = start_time.elapsed().unwrap_or_default();
                                Ok(RouteResult {
                                    success_count: 0,
                                    failed_count: 1,
                                    offline_count: 1,
                                    latency_ms: elapsed.as_millis() as u64,
                                })
                            }
                        }
                    } else {
                        // 设备不在线，存储为离线消息
                        self.store_offline_message_with_device(user_id, device_id, message).await?;
                        let elapsed = start_time.elapsed().unwrap_or_default();
                        Ok(RouteResult {
                            success_count: 0,
                            failed_count: 0,
                            offline_count: 1,
                            latency_ms: elapsed.as_millis() as u64,
                        })
                    }
                } else {
                    // 设备不存在，存储为离线消息
                    self.store_offline_message_with_device(user_id, device_id, message).await?;
                    let elapsed = start_time.elapsed().unwrap_or_default();
                    Ok(RouteResult {
                        success_count: 0,
                        failed_count: 0,
                        offline_count: 1,
                        latency_ms: elapsed.as_millis() as u64,
                    })
                }
            }
            None => {
                // 用户不在线，存储为离线消息
                self.store_offline_message_with_device(user_id, device_id, message).await?;
                let elapsed = start_time.elapsed().unwrap_or_default();
                Ok(RouteResult {
                    success_count: 0,
                    failed_count: 0,
                    offline_count: 1,
                    latency_ms: elapsed.as_millis() as u64,
                })
            }
        }
    }

    /// 批量路由消息到多个用户
    pub async fn route_message_to_users(
        &self,
        user_ids: &[UserId],
        message: PushMessageRequest,
    ) -> Result<HashMap<UserId, RouteResult>> {
        let mut results = HashMap::new();
        
        // 并发路由到所有用户
        let futures = user_ids.iter().map(|user_id| {
            let router = self;
            let message = message.clone();
            let user_id = user_id.clone();
            async move {
                let result = router.route_message_to_user(&user_id, message).await;
                (user_id, result)
            }
        });
        
        let route_results = futures::future::join_all(futures).await;
        
        for (user_id, result) in route_results {
            results.insert(user_id, result.unwrap_or_else(|_| RouteResult {
                success_count: 0,
                failed_count: 1,
                offline_count: 0,
                latency_ms: 0,
            }));
        }
        
        Ok(results)
    }

    /// 用户上线时投递离线消息
    pub async fn deliver_offline_messages(&self, user_id: &UserId, device_id: &DeviceId) -> Result<usize> {
        let offline_messages = self.offline_queue.get(user_id).await;
        
        if let Some(messages) = offline_messages {
            let mut delivered_count = 0;
            let mut remaining_messages = Vec::new();
            
            for mut offline_msg in messages {
                // 检查消息是否过期
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                if now > offline_msg.expires_at {
                    continue; // 跳过过期消息
                }
                
                // 检查是否是发送给特定设备的消息
                if let Some(target_device) = &offline_msg.target_device_id {
                    if target_device != device_id {
                        remaining_messages.push(offline_msg);
                        continue;
                    }
                }
                
                // 获取用户在线状态
                if let Some(user_status) = self.user_status_cache.get(user_id).await {
                    if let Some(device) = user_status.devices.iter().find(|d| d.device_id == *device_id) {
                        // 尝试发送消息
                        match self.session_manager.send_to_session(&device.session_id, &offline_msg.message).await {
                            Ok(_) => {
                                delivered_count += 1;
                            }
                            Err(_) => {
                                // 发送失败，增加重试次数
                                offline_msg.retry_count += 1;
                                if offline_msg.retry_count <= self.config.max_retry_count {
                                    remaining_messages.push(offline_msg);
                                }
                            }
                        }
                    } else {
                        remaining_messages.push(offline_msg);
                    }
                } else {
                    remaining_messages.push(offline_msg);
                }
            }
            
            // 更新离线消息队列
            if remaining_messages.is_empty() {
                self.offline_queue.invalidate(user_id).await;
            } else {
                self.offline_queue.put(*user_id, remaining_messages, 3600).await;
            }
            
            Ok(delivered_count)
        } else {
            Ok(0)
        }
    }

    /// 批量投递离线消息
    pub async fn deliver_offline_messages_batch(&self, user_id: &UserId, device_id: &DeviceId, batch_size: usize) -> Result<usize> {
        let offline_messages = self.offline_queue.get(user_id).await;
        
        if let Some(messages) = offline_messages {
            let mut delivered_count = 0;
            let mut remaining_messages = Vec::new();
            let mut batch_messages = Vec::new();
            
            for offline_msg in messages {
                // 检查消息是否过期
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
                if now > offline_msg.expires_at {
                    continue; // 跳过过期消息
                }
                
                // 检查是否是发送给特定设备的消息
                if let Some(target_device) = &offline_msg.target_device_id {
                    if target_device != device_id {
                        remaining_messages.push(offline_msg);
                        continue;
                    }
                }
                
                batch_messages.push(offline_msg);
                
                // 当达到批处理大小时，发送批量消息
                if batch_messages.len() >= batch_size {
                    let (delivered, remaining) = self.send_batch_messages(user_id, device_id, batch_messages).await?;
                    delivered_count += delivered;
                    remaining_messages.extend(remaining);
                    batch_messages = Vec::new();
                }
            }
            
            // 发送剩余的消息
            if !batch_messages.is_empty() {
                let (delivered, remaining) = self.send_batch_messages(user_id, device_id, batch_messages).await?;
                delivered_count += delivered;
                remaining_messages.extend(remaining);
            }
            
            // 更新离线消息队列
            if remaining_messages.is_empty() {
                self.offline_queue.invalidate(user_id).await;
            } else {
                self.offline_queue.put(*user_id, remaining_messages, 3600).await;
            }
            
            Ok(delivered_count)
        } else {
            Ok(0)
        }
    }

    /// 注册用户设备上线
    pub async fn register_device_online(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
        session_id: &SessionId,
        device_type: &str,
    ) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        let mut user_status = self.user_status_cache.get(user_id).await
            .unwrap_or_else(|| UserOnlineStatus {
                user_id: user_id.clone(),
                devices: Vec::new(),
                last_active: now,
            });
        
        // 按 session_id 更新或添加：同一 device 可能有多条连接（如 session-4 与 session-5），
        // 不能按 device_id 覆盖，否则后连上的 session 会覆盖先前的，导致离线推送找不到「先前的 session」对应设备。
        if let Some(device) = user_status.devices.iter_mut().find(|d| d.session_id == *session_id) {
            device.device_id = device_id.clone();
            device.device_type = device_type.to_string();
            device.online_time = now;
            device.last_active = now;
            device.status = DeviceStatus::Online;
        } else {
            user_status.devices.push(DeviceSession {
                device_id: device_id.clone(),
                session_id: session_id.clone(),
                device_type: device_type.to_string(),
                online_time: now,
                last_active: now,
                status: DeviceStatus::Online,
            });
        }
        
        user_status.last_active = now;
        self.user_status_cache.put(*user_id, user_status, 3600).await;
        
        Ok(())
    }

    /// 注册用户设备离线
    pub async fn register_device_offline(&self, user_id: &UserId, device_id: &DeviceId) -> Result<()> {
        if let Some(mut user_status) = self.user_status_cache.get(user_id).await {
            // 移除设备或更新状态
            user_status.devices.retain(|d| d.device_id != *device_id);
            
            if user_status.devices.is_empty() {
                // 用户完全离线
                self.user_status_cache.invalidate(user_id).await;
            } else {
                // 更新用户状态
                self.user_status_cache.put(*user_id, user_status, 3600).await;
            }
        }
        
        Ok(())
    }

    /// 获取用户在线状态
    pub async fn get_user_online_status(&self, user_id: &UserId) -> Result<Option<UserOnlineStatus>> {
        Ok(self.user_status_cache.get(user_id).await)
    }

    /// 获取路由统计信息
    pub async fn get_stats(&self) -> MessageRouterStats {
        let stats = self.stats.read().await;
        MessageRouterStats {
            total_messages: stats.total_messages,
            success_routes: stats.success_routes,
            failed_routes: stats.failed_routes,
            offline_messages: stats.offline_messages,
            avg_latency_ms: stats.avg_latency_ms,
        }
    }

    /// 清理过期离线消息
    pub async fn cleanup_expired_offline_messages(&self) -> Result<usize> {
        // 这里需要实现遍历所有用户的离线消息队列
        // 由于这是一个复杂的操作，这里只提供接口
        // 实际实现可能需要使用Redis的SCAN命令
        Ok(0)
    }

    // 私有方法

    /// 路由到在线设备
    async fn route_to_online_devices(
        &self,
        user_status: &UserOnlineStatus,
        message: PushMessageRequest,
    ) -> Result<RouteResult> {
        let start_time = SystemTime::now();
        let mut success_count = 0;
        let mut failed_count = 0;
        
        // 获取在线设备
        let online_devices: Vec<_> = user_status.devices.iter()
            .filter(|d| matches!(d.status, DeviceStatus::Online))
            .collect();
        
        if online_devices.is_empty() {
            // 没有在线设备，存储为离线消息
            self.store_offline_message(&user_status.user_id, message).await?;
            let elapsed = start_time.elapsed().unwrap_or_default();
            return Ok(RouteResult {
                success_count: 0,
                failed_count: 0,
                offline_count: 1,
                latency_ms: elapsed.as_millis() as u64,
            });
        }
        
        // 并发发送到所有在线设备
        let session_ids: Vec<_> = online_devices.iter().map(|d| d.session_id.clone()).collect();
        let results = self.session_manager.send_to_sessions(&session_ids, &message).await?;
        
        for result in results {
            match result {
                Ok(_) => success_count += 1,
                Err(_) => failed_count += 1,
            }
        }
        
        let elapsed = start_time.elapsed().unwrap_or_default();
        Ok(RouteResult {
            success_count,
            failed_count,
            offline_count: 0,
            latency_ms: elapsed.as_millis() as u64,
        })
    }

    /// 存储离线消息
    async fn store_offline_message(&self, user_id: &UserId, message: PushMessageRequest) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        use crate::infra::next_message_id;
        let offline_msg = OfflineMessage {
            message_id: next_message_id(),
            target_user_id: *user_id,
            target_device_id: None,
            message,
            created_at: now,
            expires_at: now + self.config.offline_message_ttl,
            retry_count: 0,
            priority: MessagePriority::Normal,
        };
        
        self.add_offline_message(user_id, offline_msg).await
    }

    /// 存储离线消息（指定设备）
    async fn store_offline_message_with_device(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
        message: PushMessageRequest,
    ) -> Result<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        use crate::infra::next_message_id;
        let offline_msg = OfflineMessage {
            message_id: next_message_id(),
            target_user_id: *user_id,
            target_device_id: Some(device_id.clone()),
            message,
            created_at: now,
            expires_at: now + self.config.offline_message_ttl,
            retry_count: 0,
            priority: MessagePriority::Normal,
        };
        
        self.add_offline_message(user_id, offline_msg).await
    }

    /// 添加离线消息到队列
    async fn add_offline_message(&self, user_id: &UserId, offline_msg: OfflineMessage) -> Result<()> {
        let mut offline_messages = self.offline_queue.get(user_id).await
            .unwrap_or_default();
        
        // 检查队列大小限制
        if offline_messages.len() >= self.config.max_offline_queue_size {
            // 移除最旧的消息
            offline_messages.retain(|msg| msg.created_at > offline_msg.created_at - self.config.offline_message_ttl);
        }
        
        offline_messages.push(offline_msg);
        self.offline_queue.put(*user_id, offline_messages, 3600).await;
        
        Ok(())
    }

    /// 批量发送消息
    async fn send_batch_messages(
        &self,
        user_id: &UserId,
        device_id: &DeviceId,
        messages: Vec<OfflineMessage>,
    ) -> Result<(usize, Vec<OfflineMessage>)> {
        let mut delivered_count = 0;
        let mut remaining_messages = Vec::new();
        
        // 获取用户在线状态
        if let Some(user_status) = self.user_status_cache.get(user_id).await {
            if let Some(device) = user_status.devices.iter().find(|d| d.device_id == *device_id) {
                // 准备批量消息
                let batch_msgs: Vec<_> = messages.iter().map(|m| m.message.clone()).collect();
                
                // 发送批量消息
                match self.session_manager.send_batch_to_session(&device.session_id, &batch_msgs).await {
                    Ok(_) => {
                        delivered_count = messages.len();
                    }
                    Err(_) => {
                        // 批量发送失败，增加重试次数
                        for mut msg in messages {
                            msg.retry_count += 1;
                            if msg.retry_count <= self.config.max_retry_count {
                                remaining_messages.push(msg);
                            }
                        }
                    }
                }
            } else {
                remaining_messages = messages;
            }
        } else {
            remaining_messages = messages;
        }
        
        Ok((delivered_count, remaining_messages))
    }
}

/// SessionManager 适配器
/// 将 crate::session::SessionManager 适配为 message_router::SessionManager trait
pub struct SessionManagerAdapter {
    inner: Arc<crate::session::SessionManager>,
}

impl SessionManagerAdapter {
    pub fn new(inner: Arc<crate::session::SessionManager>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl SessionManager for SessionManagerAdapter {
    async fn send_to_session(&self, _session_id: &SessionId, _message: &PushMessageRequest) -> Result<()> {
        // TODO: 实现通过 session_id 发送消息的逻辑
        // 这需要访问 TransportServer，暂时返回 Ok
        Ok(())
    }
    
    async fn send_to_sessions(&self, _sessions: &[SessionId], _message: &PushMessageRequest) -> Result<Vec<Result<()>>> {
        // TODO: 实现批量发送逻辑
        Ok(vec![])
    }
    
    async fn send_batch_to_session(&self, _session_id: &SessionId, _messages: &[PushMessageRequest]) -> Result<()> {
        // TODO: 实现批量发送逻辑
        Ok(())
    }
    
    async fn is_session_online(&self, _session_id: &SessionId) -> bool {
        // TODO: 实现会话在线检查
        // 需要访问 TransportServer 或会话状态缓存
        true
    }
    
    async fn get_online_sessions(&self) -> Vec<SessionId> {
        // TODO: 实现获取在线会话列表的逻辑
        vec![]
    }
} 