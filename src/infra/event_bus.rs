use crate::domain::events::DomainEvent;
use crate::error::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// 默认 broadcast channel 容量
const DEFAULT_CAPACITY: usize = 1000;

/// In-process Event Bus（进程内事件总线）
///
/// MVP 阶段使用 tokio::sync::broadcast
/// 未来可以替换为 Redis Streams
pub struct EventBus {
    sender: broadcast::Sender<DomainEvent>,
    /// 累计 lagged（订阅者掉队）次数
    lagged_count: Arc<AtomicU64>,
}

impl EventBus {
    /// 创建新的事件总线（默认容量 1000）
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CAPACITY)
    }

    /// 创建指定容量的事件总线
    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity);
        Self {
            sender,
            lagged_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 发布事件
    pub fn publish(&self, event: DomainEvent) -> Result<()> {
        self.sender
            .send(event)
            .map_err(|e| crate::error::ServerError::Internal(format!("Event bus error: {}", e)))?;
        Ok(())
    }

    /// 订阅事件
    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.sender.subscribe()
    }

    /// 获取 lagged 计数的引用（供订阅者记录 lagged 事件）
    pub fn lagged_counter(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.lagged_count)
    }

    /// 累计 lagged 次数
    pub fn lagged_total(&self) -> u64 {
        self.lagged_count.load(Ordering::Relaxed)
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}
