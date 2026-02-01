use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{info, warn, error};

use privchat_server::offline::{
    OfflineMessage, OfflineQueueManager, OfflineScheduler, MessagePriority,
    StorageBackend, SledStorage, scheduler::LogMessageDeliverer,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_env_filter("debug")
        .with_target(false)
        .init();

    info!("🚀 PrivChat 离线消息系统演示");

    // 演示离线消息系统
    demo_offline_message_system().await?;

    info!("✅ 演示完成");
    Ok(())
}

/// 演示离线消息系统
async fn demo_offline_message_system() -> Result<(), Box<dyn std::error::Error>> {
    info!("📬 演示 Phase 5 离线消息系统");
    
    // 创建内存存储的队列管理器
    let queue_manager = Arc::new(OfflineQueueManager::new_with_memory_storage());
    
    info!("📋 添加测试消息...");
    
    // 添加不同优先级的测试消息
    let messages = vec![
        OfflineMessage::new(
            "msg1".to_string(),
            "user1".to_string(),
            "conv1".to_string(),
            "user2".to_string(),
            "Hello from user2!".to_string(),
            "text".to_string(),
            MessagePriority::Normal,
        ),
        OfflineMessage::new(
            "msg2".to_string(),
            "user1".to_string(),
            "conv1".to_string(),
            "user3".to_string(),
            "Urgent message!".to_string(),
            "text".to_string(),
            MessagePriority::Urgent,
        ),
        OfflineMessage::new(
            "msg3".to_string(),
            "user2".to_string(),
            "conv2".to_string(),
            "user1".to_string(),
            "Low priority message".to_string(),
            "text".to_string(),
            MessagePriority::Low,
        ),
        OfflineMessage::new(
            "msg4".to_string(),
            "user2".to_string(),
            "conv1".to_string(),
            "user3".to_string(),
            "Another normal message".to_string(),
            "text".to_string(),
            MessagePriority::Normal,
        ),
        OfflineMessage::new(
            "msg5".to_string(),
            "user3".to_string(),
            "conv3".to_string(),
            "user1".to_string(),
            "High priority alert!".to_string(),
            "alert".to_string(),
            MessagePriority::High,
        ),
    ];
    
    // 添加消息到队列
    for (i, message) in messages.iter().enumerate() {
        if let Err(e) = queue_manager.add_message(message.clone()).await {
            warn!("添加消息 {} 失败: {}", i + 1, e);
        } else {
            info!("✅ 添加消息 {}: {} -> {} (优先级: {:?})", 
                  i + 1, message.sender_id, message.user_id, message.priority);
        }
    }
    
    // 显示队列统计
    let stats = queue_manager.get_stats().await;
    info!("📊 队列统计:");
    info!("  - 总消息数: {}", stats.total_messages);
    info!("  - 内存使用: {} 字节", stats.memory_usage_bytes);
    info!("  - 平均队列长度: {:.1}", stats.avg_queue_length);
    
    // 演示调度器
    demo_scheduler(queue_manager).await?;
    
    Ok(())
}

/// 演示消息调度器
async fn demo_scheduler(queue_manager: Arc<OfflineQueueManager>) -> Result<(), Box<dyn std::error::Error>> {
    info!("🔄 演示消息调度器");
    
    // 创建日志投递器
    let deliverer = Arc::new(LogMessageDeliverer::new());
    
    // 创建调度器
    let scheduler = OfflineScheduler::new(queue_manager.clone(), deliverer);
    
    info!("🎯 手动触发消息投递...");
    
    // 手动触发投递
    if let Err(e) = scheduler.trigger_delivery().await {
        error!("投递失败: {}", e);
    } else {
        info!("✅ 投递完成");
    }
    
    // 显示投递后的统计
    let stats = queue_manager.get_stats().await;
    info!("📊 投递后统计:");
    info!("  - 剩余消息数: {}", stats.total_messages);
    info!("  - 内存使用: {} 字节", stats.memory_usage_bytes);
    
    let scheduler_stats = scheduler.get_stats().await;
    info!("📊 调度器统计:");
    info!("  - 总投递次数: {}", scheduler_stats.total_deliveries);
    info!("  - 成功投递数: {}", scheduler_stats.successful_deliveries);
    info!("  - 失败投递数: {}", scheduler_stats.failed_deliveries);
    info!("  - 平均投递延迟: {:.1}ms", scheduler_stats.avg_delivery_latency_ms);
    
    // 演示自动调度
    info!("⏰ 启动自动调度器 (5秒)...");
    let scheduler_handle = scheduler.start().await?;
    
    // 运行5秒
    sleep(Duration::from_secs(5)).await;
    
    // 停止调度器
    scheduler.stop().await?;
    info!("⏹️ 调度器已停止");
    
    // 最终统计
    let final_stats = scheduler.get_stats().await;
    info!("📊 最终统计:");
    info!("  - 总投递次数: {}", final_stats.total_deliveries);
    info!("  - 成功率: {:.1}%", 
          if final_stats.total_deliveries > 0 {
              (final_stats.successful_deliveries as f64 / final_stats.total_deliveries as f64) * 100.0
          } else {
              0.0
          });
    
    Ok(())
} 