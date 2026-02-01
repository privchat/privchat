use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use chrono::{DateTime, Utc};
use tokio::time::interval;
use tracing::{debug, info, warn, error};
use crate::error::ServerError;
use super::storage::StorageBackend;
use super::queue::OfflineQueueManager;

/// 调度器配置
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// 投递检查间隔（秒）
    pub delivery_check_interval_secs: u64,
    /// 批量投递大小
    pub batch_delivery_size: usize,
    /// 最大重试次数
    pub max_retry_count: usize,
    /// 重试延迟（秒）
    pub retry_delay_secs: u64,
    /// 并发投递任务数
    pub max_concurrent_deliveries: usize,
    /// 失败重试间隔（秒）
    pub retry_check_interval_secs: u64,
    /// 统计报告间隔（秒）
    pub stats_report_interval_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            delivery_check_interval_secs: 10,
            batch_delivery_size: 50,
            max_retry_count: 3,
            retry_delay_secs: 30,
            max_concurrent_deliveries: 10,
            retry_check_interval_secs: 60,
            stats_report_interval_secs: 300,
        }
    }
}

/// 投递结果
#[derive(Debug, Clone)]
pub struct DeliveryResult {
    pub message_id: u64,
    pub success: bool,
    pub error: Option<String>,
}

/// 调度器统计信息
#[derive(Debug, Clone)]
pub struct SchedulerStats {
    pub total_delivered: u64,
    pub total_failed: u64,
    pub active_deliveries: u64,
    pub pending_retries: u64,
    pub last_delivery_time: Option<DateTime<Utc>>,
}

/// 消息投递器 Trait
#[async_trait::async_trait]
pub trait MessageDeliverer: Send + Sync + 'static {
    /// 投递消息到用户
    async fn deliver_message(&self, user_id: u64, message: &super::message::OfflineMessage) -> Result<(), ServerError>;
    
    /// 批量投递消息
    async fn deliver_messages(&self, user_id: u64, messages: &[super::message::OfflineMessage]) -> Vec<DeliveryResult> {
        let mut results = Vec::new();
        
        for message in messages {
            let result = match self.deliver_message(user_id, message).await {
                Ok(()) => DeliveryResult {
                    message_id: message.message_id,
                    success: true,
                    error: None,
                },
                Err(e) => DeliveryResult {
                    message_id: message.message_id,
                    success: false,
                    error: Some(e.to_string()),
                },
            };
            results.push(result);
        }
        
        results
    }
}

/// 简单的日志投递器（用于测试）
#[derive(Debug, Clone)]
pub struct LogMessageDeliverer;

#[async_trait::async_trait]
impl MessageDeliverer for LogMessageDeliverer {
    async fn deliver_message(&self, user_id: u64, message: &super::message::OfflineMessage) -> Result<(), ServerError> {
        info!("Delivering message {} to user {} (from: {}, type: {})", 
              message.message_id, user_id, message.sender_id, message.message_type);
        
        // 模拟投递延迟
        tokio::time::sleep(Duration::from_millis(10)).await;
        
        // 模拟 10% 的失败率
        if rand::random::<f32>() < 0.1 {
            return Err(ServerError::Internal("Simulated delivery failure".to_string()));
        }
        
        Ok(())
    }
}

/// 离线消息调度器
pub struct OfflineScheduler<S: StorageBackend + Send + Sync + 'static, D: MessageDeliverer> {
    /// 队列管理器
    queue_manager: Arc<OfflineQueueManager<S>>,
    /// 消息投递器
    message_deliverer: Arc<D>,
    /// 配置
    config: SchedulerConfig,
    /// 统计信息
    stats: Arc<tokio::sync::Mutex<SchedulerStats>>,
    /// 运行状态
    running: Arc<tokio::sync::Mutex<bool>>,
    /// 活跃投递任务
    active_deliveries: Arc<tokio::sync::Mutex<HashMap<u64, tokio::task::JoinHandle<()>>>>,
}

impl<S: StorageBackend + Send + Sync + 'static, D: MessageDeliverer> OfflineScheduler<S, D> {
    /// 创建新的调度器
    pub fn new(
        queue_manager: Arc<OfflineQueueManager<S>>,
        message_deliverer: Arc<D>,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            queue_manager,
            message_deliverer,
            config,
            stats: Arc::new(tokio::sync::Mutex::new(SchedulerStats {
                total_delivered: 0,
                total_failed: 0,
                active_deliveries: 0,
                pending_retries: 0,
                last_delivery_time: None,
            })),
            running: Arc::new(tokio::sync::Mutex::new(false)),
            active_deliveries: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// 启动调度器
    pub async fn start(&self) -> Result<(), ServerError> {
        let mut running = self.running.lock().await;
        if *running {
            return Err(ServerError::Internal("Scheduler is already running".to_string()));
        }
        *running = true;

        info!("Starting offline message scheduler");
        
        // 启动主投递循环
        self.start_delivery_loop().await;
        
        // 启动统计报告任务
        self.start_stats_reporter().await;
        
        // 启动重试任务
        self.start_retry_loop().await;

        Ok(())
    }

    /// 停止调度器
    pub async fn stop(&self) -> Result<(), ServerError> {
        let mut running = self.running.lock().await;
        if !*running {
            return Ok(());
        }
        *running = false;

        info!("Stopping offline message scheduler");
        
        // 等待所有活跃投递完成
        let mut deliveries = self.active_deliveries.lock().await;
        for (user_id, handle) in deliveries.drain() {
            debug!("Waiting for delivery task for user {} to complete", user_id);
            let _ = handle.await;
        }

        Ok(())
    }

    /// 手动触发用户消息投递
    pub async fn trigger_delivery_for_user(&self, user_id: u64) -> Result<usize, ServerError> {
        let messages = self.queue_manager.get_messages_for_delivery(user_id, Some(self.config.batch_delivery_size)).await?;
        
        if messages.is_empty() {
            return Ok(0);
        }

        let delivered_count = self.deliver_messages_to_user(user_id, &messages).await?;
        Ok(delivered_count)
    }

    /// 获取调度器统计信息
    pub async fn get_stats(&self) -> SchedulerStats {
        self.stats.lock().await.clone()
    }

    /// 投递消息给指定用户
    async fn deliver_messages_to_user(&self, user_id: u64, messages: &[super::message::OfflineMessage]) -> Result<usize, ServerError> {
        if messages.is_empty() {
            return Ok(0);
        }

        let start_time = std::time::Instant::now();
        let results = self.message_deliverer.deliver_messages(user_id, messages).await;
        
        let mut delivered_ids = Vec::new();
        let mut failed_count = 0;

        for result in results {
            if result.success {
                delivered_ids.push(result.message_id);
            } else {
                failed_count += 1;
                warn!("Failed to deliver message {} to user {}: {:?}", 
                      result.message_id, user_id, result.error);
                
                // 标记失败（这里简化处理，实际应该有重试逻辑）
                let _ = self.queue_manager.mark_failed(user_id, result.message_id).await;
            }
        }

        // 标记成功投递的消息
        if !delivered_ids.is_empty() {
            let _ = self.queue_manager.mark_delivered(user_id, &delivered_ids).await;
        }

        // 更新统计信息
        {
            let mut stats = self.stats.lock().await;
            stats.total_delivered += delivered_ids.len() as u64;
            stats.total_failed += failed_count;
            stats.last_delivery_time = Some(Utc::now());
        }

        let _delivery_time = start_time.elapsed();
        debug!("Delivered {} messages to user {} ({} failed)", 
               delivered_ids.len(), user_id, failed_count);

        Ok(delivered_ids.len())
    }

    /// 启动主投递循环
    async fn start_delivery_loop(&self) {
        let queue_manager = self.queue_manager.clone();
        let message_deliverer = self.message_deliverer.clone();
        let config = self.config.clone();
        let stats = self.stats.clone();
        let running = self.running.clone();
        let active_deliveries = self.active_deliveries.clone();

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(config.delivery_check_interval_secs));
            
            loop {
                interval.tick().await;
                
                // 检查是否还在运行
                {
                    let running_guard = running.lock().await;
                    if !*running_guard {
                        break;
                    }
                }

                // 获取有消息的用户列表（这里简化，实际应该从队列管理器获取）
                let users_with_messages: Vec<u64> = vec![]; // TODO: 从队列管理器获取实际用户列表
                
                for user_id in users_with_messages {
                    let mut deliveries = active_deliveries.lock().await;
                    
                    // 检查是否已经有该用户的投递任务在运行
                    if deliveries.contains_key(&user_id) {
                        continue;
                    }

                    // 检查并发投递限制
                    if deliveries.len() >= config.max_concurrent_deliveries {
                        continue;
                    }

                    let queue_manager_clone = queue_manager.clone();
                    let deliverer_clone = message_deliverer.clone();
                    let user_id_clone = user_id;
                    let _config_clone = config.clone();
                    let stats_clone = stats.clone();
                    let batch_size = config.batch_delivery_size;

                    let task = tokio::spawn(async move {
                        match queue_manager_clone.get_messages_for_delivery(user_id_clone, Some(batch_size)).await {
                            Ok(messages) => {
                                if !messages.is_empty() {
                                    let results = deliverer_clone.deliver_messages(user_id_clone, &messages).await;
                                    
                                    let mut delivered_ids = Vec::new();
                                    let mut failed_count = 0;

                                    for result in results {
                                        if result.success {
                                            delivered_ids.push(result.message_id);
                                        } else {
                                            failed_count += 1;
                                            let _ = queue_manager_clone.mark_failed(user_id_clone, result.message_id).await;
                                        }
                                    }

                                    if !delivered_ids.is_empty() {
                                        let _ = queue_manager_clone.mark_delivered(user_id_clone, &delivered_ids).await;
                                    }

                                    // 更新统计
                                    {
                                        let mut stats_guard = stats_clone.lock().await;
                                        stats_guard.total_delivered += delivered_ids.len() as u64;
                                        stats_guard.total_failed += failed_count;
                                        stats_guard.last_delivery_time = Some(Utc::now());
                                    }
                                }
                            },
                            Err(e) => {
                                error!("Failed to get messages for user {}: {:?}", user_id_clone, e);
                            }
                        }
                    });

                    deliveries.insert(user_id, task);
                }

                // 清理已完成的任务
                let mut deliveries = active_deliveries.lock().await;
                deliveries.retain(|_user_id, handle| !handle.is_finished());
            }
        });
    }

    /// 启动统计报告任务
    async fn start_stats_reporter(&self) {
        let _queue_manager = self.queue_manager.clone();
        let stats = self.stats.clone();
        let running = self.running.clone();
        let interval_secs = self.config.stats_report_interval_secs;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_secs));
            
            loop {
                interval.tick().await;
                
                {
                    let running_guard = running.lock().await;
                    if !*running_guard {
                        break;
                    }
                }

                let stats_snapshot = stats.lock().await.clone();
                info!("Scheduler stats: delivered={}, failed={}, active={}, pending_retries={}", 
                      stats_snapshot.total_delivered,
                      stats_snapshot.total_failed,
                      stats_snapshot.active_deliveries,
                      stats_snapshot.pending_retries);
            }
        });
    }

    /// 启动重试循环
    async fn start_retry_loop(&self) {
        let _queue_manager = self.queue_manager.clone();
        let _message_deliverer = self.message_deliverer.clone();
        let running = self.running.clone();
        let interval_secs = self.config.retry_check_interval_secs;

        tokio::spawn(async move {
            let mut interval = interval(Duration::from_secs(interval_secs));
            
            loop {
                interval.tick().await;
                
                {
                    let running_guard = running.lock().await;
                    if !*running_guard {
                        break;
                    }
                }

                // 这里可以实现重试逻辑
                // 例如：查找失败的消息，重新尝试投递
                debug!("Checking for messages to retry...");
                
                // 示例：获取失败消息并重试
                // let failed_messages = queue_manager.get_failed_messages().await;
                // for message in failed_messages {
                //     // 重试逻辑
                // }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::queue::create_memory_queue_manager;
    use crate::infra::OnlineStatusManager;
    use bytes::Bytes;

    #[tokio::test]
    async fn test_scheduler_basic_operations() {
        let queue_manager = Arc::new(create_memory_queue_manager(None).await.unwrap());
        let online_status_manager = Arc::new(OnlineStatusManager::new());
        let message_deliverer = Arc::new(LogMessageDeliverer);
        let config = SchedulerConfig::default();
        
        let scheduler = OfflineScheduler::new(
            queue_manager.clone(),
            online_status_manager.clone(),
            message_deliverer,
            config,
        );
        
        // 启动队列管理器和调度器
        queue_manager.start().await.unwrap();
        scheduler.start().await.unwrap();
        
        // 模拟用户上线
        online_status_manager.simple_user_online(
            "session1".to_string(),
            "user1".to_string(),
            "device1".to_string(),
            privchat_protocol::DeviceType::iOS,
            "127.0.0.1".to_string(),
        );
        
        // 添加离线消息
        let message_id = queue_manager.add_message(
            "user1".to_string(),
            "sender1".to_string(),
            "conv1".to_string(),
            Bytes::from("Hello"),
            "text".to_string(),
            None,
        ).await.unwrap();
        
        // 手动触发投递
        let delivered = scheduler.trigger_delivery_for_user("user1").await.unwrap();
        assert_eq!(delivered, 1);
        
        // 检查统计信息
        let stats = scheduler.get_stats().await;
        assert_eq!(stats.total_delivered, 1);
        assert_eq!(stats.total_failed, 0);
        
        scheduler.stop().await.unwrap();
        queue_manager.stop().await.unwrap();
    }

    #[tokio::test]
    async fn test_scheduler_with_failures() {
        let queue_manager = Arc::new(create_memory_queue_manager(None).await.unwrap());
        let online_status_manager = Arc::new(OnlineStatusManager::new());
        let message_deliverer = Arc::new(LogMessageDeliverer);
        let config = SchedulerConfig::default();
        
        let scheduler = OfflineScheduler::new(
            queue_manager.clone(),
            online_status_manager.clone(),
            message_deliverer,
            config,
        );
        
        queue_manager.start().await.unwrap();
        scheduler.start().await.unwrap();
        
        // 模拟用户上线
        online_status_manager.simple_user_online(
            "session1".to_string(),
            "user1".to_string(),
            "device1".to_string(),
            privchat_protocol::DeviceType::iOS,
            "127.0.0.1".to_string(),
        );
        
        // 添加多条消息
        for i in 1..=10 {
            queue_manager.add_message(
                "user1".to_string(),
                "sender1".to_string(),
                "conv1".to_string(),
                Bytes::from(format!("Message {}", i)),
                "text".to_string(),
                None,
            ).await.unwrap();
        }
        
        // 手动触发投递
        let delivered = scheduler.trigger_delivery_for_user("user1").await.unwrap();
        
        // 检查统计信息
        let stats = scheduler.get_stats().await;
        assert!(stats.total_delivered > 0);
        assert!(stats.total_failed > 0);
        assert_eq!(stats.total_delivered + stats.total_failed, 10);
        
        scheduler.stop().await.unwrap();
        queue_manager.stop().await.unwrap();
    }
} 