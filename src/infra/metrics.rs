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
