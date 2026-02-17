//! 业务 Handler 全局并发限流器
//!
//! 限制同时运行的业务处理任务数量，防止 task 爆炸导致 OOM。
//! 连接层（accept/read loop/heartbeat）不受限制。
//!
//! 参考：STABILITY_SPEC 禁令 2 + MESSAGE_SPEC 9.2

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// 业务 Handler 并发限流器
#[derive(Clone)]
pub struct HandlerLimiter {
    sem: Arc<Semaphore>,
    max_inflight: usize,
    /// try_acquire 失败计数（handler_rejected_total）
    rejected_count: Arc<AtomicU64>,
}

impl HandlerLimiter {
    pub fn new(max_inflight: usize) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(max_inflight)),
            max_inflight,
            rejected_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// 尝试获取 permit（非阻塞）。
    /// 成功返回 Ok(permit)，失败返回 Err(()) 并自增 rejected 计数。
    ///
    /// 推荐用于消息处理入口：
    /// - 成功：spawn task 并在 task 内持有 permit
    /// - 失败：触发降级（计数 + 错误响应）
    pub fn try_acquire(&self) -> Result<OwnedSemaphorePermit, ()> {
        match self.sem.clone().try_acquire_owned() {
            Ok(permit) => Ok(permit),
            Err(_) => {
                self.rejected_count.fetch_add(1, Ordering::Relaxed);
                Err(())
            }
        }
    }

    /// 当前正在执行的 handler 数量（handler_inflight gauge）
    pub fn inflight(&self) -> usize {
        self.max_inflight - self.sem.available_permits()
    }

    /// 剩余可用 permit 数
    pub fn available_permits(&self) -> usize {
        self.sem.available_permits()
    }

    /// 累计被拒绝的请求数（handler_rejected_total counter）
    pub fn rejected_total(&self) -> u64 {
        self.rejected_count.load(Ordering::Relaxed)
    }

    /// 最大并发数配置
    pub fn max_inflight(&self) -> usize {
        self.max_inflight
    }
}
