//! 消息投递追踪（DeliveryTrace）
//!
//! 以 server_msg_id 为关联键，记录消息在投递管道中经过的每个节点和终态。
//! 内存存储，有界 LRU，仅用于可观测性和调试，不参与业务逻辑。
//!
//! 6 个追踪节点：
//! 1. committed  — sync/submit 写入 commit_log 成功
//! 2. fanout     — 在线用户扇出（Redis PUBLISH）
//! 3. push_sent  — 推送通知发送成功
//! 4. push_failed — 推送通知发送失败
//! 5. offline_enqueued — 消息入离线队列
//! 6. catchup_delivered — 离线补推送达

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;

/// 投递终态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceTerminalState {
    /// 已投递（至少一个接收端确认）
    Delivered,
    /// 投递失败（所有尝试均失败）
    Failed,
    /// 已取消（消息撤销导致）
    Cancelled,
    /// 超时（超过 TTL 未投递）
    TimedOut,
}

/// 单个追踪节点
#[derive(Debug, Clone)]
pub struct TraceNode {
    /// 节点名称
    pub stage: &'static str,
    /// 时间戳（毫秒）
    pub timestamp_ms: i64,
    /// 附加信息
    pub detail: String,
}

/// 单条消息的投递追踪记录
#[derive(Debug, Clone)]
pub struct DeliveryTrace {
    /// 消息 ID（Snowflake）
    pub server_msg_id: u64,
    /// 频道 ID
    pub channel_id: u64,
    /// 发送者 ID
    pub sender_id: u64,
    /// 创建时间
    pub created_at_ms: i64,
    /// 追踪节点列表（按时间顺序）
    pub nodes: Vec<TraceNode>,
    /// 终态（None 表示仍在投递中）
    pub terminal_state: Option<TraceTerminalState>,
    /// 终态时间
    pub terminal_at_ms: Option<i64>,
}

impl DeliveryTrace {
    /// 创建新的追踪记录
    pub fn new(server_msg_id: u64, channel_id: u64, sender_id: u64) -> Self {
        Self {
            server_msg_id,
            channel_id,
            sender_id,
            created_at_ms: chrono::Utc::now().timestamp_millis(),
            nodes: Vec::with_capacity(4),
            terminal_state: None,
            terminal_at_ms: None,
        }
    }

    /// 添加追踪节点
    fn add_node(&mut self, stage: &'static str, detail: String) {
        self.nodes.push(TraceNode {
            stage,
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            detail,
        });
    }

    /// 设置终态
    fn set_terminal(&mut self, state: TraceTerminalState) {
        if self.terminal_state.is_none() {
            self.terminal_state = Some(state);
            self.terminal_at_ms = Some(chrono::Utc::now().timestamp_millis());
        }
    }

    /// 从创建到终态的总耗时（毫秒），未完成返回 None
    pub fn total_latency_ms(&self) -> Option<i64> {
        self.terminal_at_ms.map(|t| t - self.created_at_ms)
    }
}

/// 全局追踪存储（有界 LRU）
pub struct TraceStore {
    /// 按 server_msg_id 索引
    traces: Mutex<HashMap<u64, DeliveryTrace>>,
    /// 最大容量
    max_capacity: usize,
    /// 插入顺序（用于淘汰）
    order: Mutex<Vec<u64>>,
}

impl TraceStore {
    /// 创建追踪存储
    pub fn new(max_capacity: usize) -> Self {
        Self {
            traces: Mutex::new(HashMap::with_capacity(max_capacity)),
            max_capacity,
            order: Mutex::new(Vec::with_capacity(max_capacity)),
        }
    }

    /// 创建并注册一条新追踪
    pub async fn begin_trace(&self, server_msg_id: u64, channel_id: u64, sender_id: u64) {
        let trace = DeliveryTrace::new(server_msg_id, channel_id, sender_id);
        let mut traces = self.traces.lock().await;
        let mut order = self.order.lock().await;

        // LRU 淘汰
        while traces.len() >= self.max_capacity && !order.is_empty() {
            let oldest = order.remove(0);
            traces.remove(&oldest);
        }

        traces.insert(server_msg_id, trace);
        order.push(server_msg_id);
    }

    /// 记录追踪节点
    pub async fn record(&self, server_msg_id: u64, stage: &'static str, detail: String) {
        let mut traces = self.traces.lock().await;
        if let Some(trace) = traces.get_mut(&server_msg_id) {
            trace.add_node(stage, detail.clone());
            debug!(
                "[TRACE] msg_id={} stage={} detail={}",
                server_msg_id, stage, detail
            );
        }
    }

    /// 设置终态
    pub async fn set_terminal(&self, server_msg_id: u64, state: TraceTerminalState) {
        let mut traces = self.traces.lock().await;
        if let Some(trace) = traces.get_mut(&server_msg_id) {
            trace.set_terminal(state);
            debug!(
                "[TRACE] msg_id={} terminal={:?} latency={:?}ms",
                server_msg_id,
                state,
                trace.total_latency_ms()
            );
        }
    }

    /// 查询单条追踪
    pub async fn get(&self, server_msg_id: u64) -> Option<DeliveryTrace> {
        let traces = self.traces.lock().await;
        traces.get(&server_msg_id).cloned()
    }

    /// 当前追踪数量
    pub async fn len(&self) -> usize {
        let traces = self.traces.lock().await;
        traces.len()
    }

    /// 统计：最近 N 条的平均投递延迟（仅已完成的）
    pub async fn avg_delivery_latency_ms(&self, limit: usize) -> Option<f64> {
        let traces = self.traces.lock().await;
        let order = self.order.lock().await;

        let mut latencies = Vec::new();
        for msg_id in order.iter().rev().take(limit) {
            if let Some(trace) = traces.get(msg_id) {
                if let Some(latency) = trace.total_latency_ms() {
                    latencies.push(latency as f64);
                }
            }
        }

        if latencies.is_empty() {
            None
        } else {
            let sum: f64 = latencies.iter().sum();
            Some(sum / latencies.len() as f64)
        }
    }
}

/// 全局 TraceStore 单例
static GLOBAL_TRACE_STORE: std::sync::OnceLock<Arc<TraceStore>> = std::sync::OnceLock::new();

/// 初始化全局追踪存储
pub fn init_global_trace_store(max_capacity: usize) {
    let _ = GLOBAL_TRACE_STORE.set(Arc::new(TraceStore::new(max_capacity)));
}

/// 获取全局追踪存储
pub fn global_trace_store() -> &'static Arc<TraceStore> {
    GLOBAL_TRACE_STORE.get_or_init(|| Arc::new(TraceStore::new(10_000)))
}

// ============================================================
// 便捷宏：在各节点插入 trace
// ============================================================

/// 追踪节点常量
pub mod stages {
    pub const COMMITTED: &str = "committed";
    pub const FANOUT: &str = "fanout";
    pub const PUSH_SENT: &str = "push_sent";
    pub const PUSH_FAILED: &str = "push_failed";
    pub const OFFLINE_ENQUEUED: &str = "offline_enqueued";
    pub const CATCHUP_DELIVERED: &str = "catchup_delivered";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_trace_lifecycle() {
        let store = TraceStore::new(100);

        // begin
        store.begin_trace(1001, 42, 7).await;

        // record nodes
        store
            .record(1001, stages::COMMITTED, "pts=5".to_string())
            .await;
        store
            .record(1001, stages::FANOUT, "online_users=3".to_string())
            .await;
        store
            .record(1001, stages::PUSH_SENT, "device=abc".to_string())
            .await;

        // terminal
        store
            .set_terminal(1001, TraceTerminalState::Delivered)
            .await;

        // verify
        let trace = store.get(1001).await.unwrap();
        assert_eq!(trace.server_msg_id, 1001);
        assert_eq!(trace.channel_id, 42);
        assert_eq!(trace.sender_id, 7);
        assert_eq!(trace.nodes.len(), 3);
        assert_eq!(trace.nodes[0].stage, "committed");
        assert_eq!(trace.nodes[1].stage, "fanout");
        assert_eq!(trace.nodes[2].stage, "push_sent");
        assert_eq!(trace.terminal_state, Some(TraceTerminalState::Delivered));
        assert!(trace.total_latency_ms().is_some());
    }

    #[tokio::test]
    async fn test_trace_lru_eviction() {
        let store = TraceStore::new(3);

        store.begin_trace(1, 1, 1).await;
        store.begin_trace(2, 1, 1).await;
        store.begin_trace(3, 1, 1).await;

        assert_eq!(store.len().await, 3);

        // 第 4 条应淘汰第 1 条
        store.begin_trace(4, 1, 1).await;
        assert_eq!(store.len().await, 3);
        assert!(store.get(1).await.is_none());
        assert!(store.get(4).await.is_some());
    }

    #[tokio::test]
    async fn test_trace_not_found() {
        let store = TraceStore::new(10);
        // record 对不存在的 msg_id 应无副作用
        store
            .record(9999, stages::COMMITTED, "test".to_string())
            .await;
        assert!(store.get(9999).await.is_none());
    }
}
