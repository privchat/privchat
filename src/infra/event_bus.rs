use tokio::sync::broadcast;
use crate::domain::events::DomainEvent;
use crate::error::Result;

/// In-process Event Bus（进程内事件总线）
/// 
/// MVP 阶段使用 tokio::sync::broadcast
/// 未来可以替换为 Redis Streams
pub struct EventBus {
    sender: broadcast::Sender<DomainEvent>,
}

impl EventBus {
    /// 创建新的事件总线
    pub fn new() -> Self {
        let (sender, _) = broadcast::channel(1000);
        Self { sender }
    }
    
    /// 发布事件
    pub fn publish(&self, event: DomainEvent) -> Result<()> {
        self.sender.send(event).map_err(|e| {
            crate::error::ServerError::Internal(format!("Event bus error: {}", e))
        })?;
        Ok(())
    }
    
    /// 订阅事件
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.sender.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
