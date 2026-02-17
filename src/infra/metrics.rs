//! Prometheus 指标：连接数、RPC 调用量与延迟、消息发送量等
//!
//! 通过 `init()` 安装全局 Recorder，通过 HTTP GET `/metrics` 暴露抓取端点。

use metrics_exporter_prometheus::PrometheusHandle;
use std::sync::OnceLock;

static HANDLE: OnceLock<PrometheusHandle> = OnceLock::new();

/// 指标名称
const GAUGE_CONNECTIONS: &str = "privchat_connections_current";
const COUNTER_RPC_TOTAL: &str = "privchat_rpc_total";
const HISTOGRAM_RPC_DURATION: &str = "privchat_rpc_duration_seconds";
const COUNTER_MESSAGES_SENT: &str = "privchat_messages_sent_total";
const GAUGE_HANDLER_INFLIGHT: &str = "privchat_handler_inflight";
const COUNTER_HANDLER_REJECTED: &str = "privchat_handler_rejected_total";
const COUNTER_EVENT_BUS_LAGGED: &str = "privchat_event_bus_lagged_total";
const GAUGE_REDIS_POOL_ACTIVE: &str = "privchat_redis_pool_active";
const GAUGE_REDIS_POOL_IDLE: &str = "privchat_redis_pool_idle";
const GAUGE_DB_POOL_ACTIVE: &str = "privchat_db_pool_active";
const GAUGE_DB_POOL_IDLE: &str = "privchat_db_pool_idle";
const GAUGE_OFFLINE_QUEUE_DEPTH: &str = "privchat_offline_queue_depth";
const COUNTER_OFFLINE_TRY_SEND_FAIL: &str = "privchat_offline_try_send_fail_total";
const COUNTER_OFFLINE_FALLBACK: &str = "privchat_offline_fallback_total";

/// 初始化 Prometheus 指标（安装全局 Recorder，返回 Handle 用于 HTTP 暴露）。
/// 仅需在进程内调用一次；重复调用会返回 Err。
pub fn init() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let handle = metrics_exporter_prometheus::PrometheusBuilder::new().install_recorder()?;
    HANDLE
        .set(handle)
        .map_err(|_| "metrics already initialized")?;
    Ok(())
}

/// 是否已初始化（可供 /metrics 使用）
pub fn is_initialized() -> bool {
    HANDLE.get().is_some()
}

/// 渲染当前指标为 Prometheus 文本格式，供 GET /metrics 使用。
pub fn render_metrics() -> Option<String> {
    HANDLE.get().map(|h| h.render())
}

/// 更新当前连接数（Gauge）。在连接注册/注销后调用。
pub fn record_connection_count(count: u64) {
    metrics::gauge!(GAUGE_CONNECTIONS).set(count as f64);
}

/// 记录一次 RPC 调用：总次数 + 耗时直方图。
pub fn record_rpc(route: &str, duration_secs: f64) {
    metrics::counter!(COUNTER_RPC_TOTAL, "route" => route.to_string()).increment(1);
    metrics::histogram!(HISTOGRAM_RPC_DURATION, "route" => route.to_string()).record(duration_secs);
}

/// 记录发送消息数 +1。
pub fn record_message_sent() {
    metrics::counter!(COUNTER_MESSAGES_SENT).increment(1);
}

/// 更新当前 handler 并发数（Gauge）。由定时任务周期调用。
pub fn record_handler_inflight(count: usize) {
    metrics::gauge!(GAUGE_HANDLER_INFLIGHT).set(count as f64);
}

/// 记录 handler 被限流拒绝次数（Counter）。
pub fn record_handler_rejected(count: u64) {
    metrics::counter!(COUNTER_HANDLER_REJECTED).absolute(count);
}

/// 记录 EventBus lagged 次数（Counter）。
pub fn record_event_bus_lagged(count: u64) {
    metrics::counter!(COUNTER_EVENT_BUS_LAGGED).absolute(count);
}

/// 更新 Redis 连接池状态（Gauge）。
pub fn record_redis_pool(active: u32, idle: u32) {
    metrics::gauge!(GAUGE_REDIS_POOL_ACTIVE).set(active as f64);
    metrics::gauge!(GAUGE_REDIS_POOL_IDLE).set(idle as f64);
}

/// 更新数据库连接池状态（Gauge）。
pub fn record_db_pool(active: u32, idle: u32) {
    metrics::gauge!(GAUGE_DB_POOL_ACTIVE).set(active as f64);
    metrics::gauge!(GAUGE_DB_POOL_IDLE).set(idle as f64);
}

/// 更新离线队列深度（Gauge）。
pub fn record_offline_queue_depth(depth: usize) {
    metrics::gauge!(GAUGE_OFFLINE_QUEUE_DEPTH).set(depth as f64);
}

/// 记录离线队列 try_send 失败次数（Counter）。
pub fn record_offline_try_send_fail(count: u64) {
    metrics::counter!(COUNTER_OFFLINE_TRY_SEND_FAIL).absolute(count);
}

/// 记录离线队列降级（fallback）次数（Counter）。
pub fn record_offline_fallback(count: u64) {
    metrics::counter!(COUNTER_OFFLINE_FALLBACK).absolute(count);
}
