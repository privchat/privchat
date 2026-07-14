// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
/// 建连/认证 server 侧耗时直方图（G8 归因，见 LOAD_GATE_EXECUTION_SPEC §7）。无高基数 label。
/// 注意：**只覆盖 server app 层收到 `ConnectionEstablished` transport event 后的处理耗时**
/// （register_connecting + IP 安全检查 + stats），**不含** TCP/QUIC 握手本身（那在 msgtrans）。
const HISTOGRAM_CONN_ESTABLISHED_HANDLING: &str =
    "privchat_connection_established_handling_seconds";
const HISTOGRAM_AUTHENTICATE_DURATION: &str = "privchat_authenticate_duration_seconds";
/// 建连/认证延迟 bucket：覆盖 10ms→10s。
const CONN_AUTH_BUCKETS: &[f64] = &[0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];
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

// ---------------------------------------------------------------------------
// CONNECTION_LIFECYCLE_SPEC 观测指标
// ---------------------------------------------------------------------------

/// 当前在线用户数（Index A 不同 user_id 数，Authenticated）
const GAUGE_CONN_ONLINE_USERS: &str = "privchat_connection_online_users";
/// 当前在线 session 数（Index B 状态为 Authenticated 的条目数）
const GAUGE_CONN_ONLINE_SESSIONS: &str = "privchat_connection_online_sessions";
/// 累计 replaced session 数（同设备新连接接管时 +1）
const COUNTER_CONN_REPLACED: &str = "privchat_connection_replaced_sessions_total";
/// 累计 Connecting 超时清理数（spec §5 GC）
const COUNTER_CONN_CONNECTING_TIMEOUT: &str = "privchat_connection_connecting_timeout_total";

/// 累计投递尝试数（spec §6.1 入口调用）
const COUNTER_DELIVERY_ATTEMPT: &str = "privchat_delivery_attempt_total";
/// 累计成功投递的 session 次数（每成功送到一个 session +1）
const COUNTER_DELIVERY_SUCCESS_SESSIONS: &str = "privchat_delivery_success_sessions_total";
/// 累计投递尝试但成功数 = 0 的次数（spec §6.3 触发离线落盘的前置信号）
const COUNTER_DELIVERY_ZERO_SUCCESS: &str = "privchat_delivery_zero_success_total";
/// 累计写入离线队列次数（success_count == 0 时触发）
const COUNTER_OFFLINE_ENQUEUE: &str = "privchat_offline_enqueue_total";
/// 累计被 A→B 二次校验过滤掉的 session 次数（带 reason label）
const COUNTER_DELIVERY_FILTERED: &str = "privchat_delivery_filtered_total";

/// 初始化 Prometheus 指标（安装全局 Recorder，返回 Handle 用于 HTTP 暴露）。
/// 仅需在进程内调用一次；重复调用会返回 Err。
pub fn init() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use metrics_exporter_prometheus::Matcher;
    // 为建连/认证直方图配显式 bucket（10ms→10s）；其它指标保持默认。
    let handle = metrics_exporter_prometheus::PrometheusBuilder::new()
        .set_buckets_for_metric(
            Matcher::Full(HISTOGRAM_CONN_ESTABLISHED_HANDLING.to_string()),
            CONN_AUTH_BUCKETS,
        )
        .map_err(|e| format!("connect bucket config: {e}"))?
        .set_buckets_for_metric(
            Matcher::Full(HISTOGRAM_AUTHENTICATE_DURATION.to_string()),
            CONN_AUTH_BUCKETS,
        )
        .map_err(|e| format!("authenticate bucket config: {e}"))?
        .install_recorder()?;
    HANDLE
        .set(handle)
        .map_err(|_| "metrics already initialized")?;
    preregister_rpc_result();
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

/// RPC 请求结果计数（低基数 result label：ok|error）。供 G10 soak 判 **RPC** 错误率（§3.1）。
/// 作用域仅 RPC dispatch（覆盖 login/submit/sync 等所有 RPC route，code!=0 计 error）；
/// 限流拒绝(busy)不在此计（单独 `privchat_handler_rejected_total`），避免分子分母作用域不一致。
pub fn record_rpc_result(result: &str) {
    metrics::counter!("privchat_rpc_requests_total", "result" => result.to_string()).increment(1);
}

/// 预注册 rpc_requests_total{ok,error}=0（进程启动即可见，避免干净 server 上「缺 metric family」）。
fn preregister_rpc_result() {
    metrics::counter!("privchat_rpc_requests_total", "result" => "ok").increment(0);
    metrics::counter!("privchat_rpc_requests_total", "result" => "error").increment(0);
}

/// tokio 存活 task 数（Gauge）。供 G10 soak 判 task 泄漏（§3.1）。
/// `num_alive_tasks` 自 tokio 1.38 稳定，无需 tokio_unstable。
pub fn record_tokio_alive_tasks(count: u64) {
    metrics::gauge!("privchat_tokio_alive_tasks").set(count as f64);
}

/// 启动后台 sampler：每 `interval_secs` 采一次 tokio 存活 task 数写入 gauge。
/// 在进程 runtime 内调用一次即可。
pub fn spawn_tokio_task_sampler(interval_secs: u64) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
        loop {
            tick.tick().await;
            let n = tokio::runtime::Handle::current().metrics().num_alive_tasks() as u64;
            record_tokio_alive_tasks(n);
        }
    });
}

/// 记录一次 RPC 调用：总次数 + 耗时直方图。
pub fn record_rpc(route: &str, duration_secs: f64) {
    metrics::counter!(COUNTER_RPC_TOTAL, "route" => route.to_string()).increment(1);
    metrics::histogram!(HISTOGRAM_RPC_DURATION, "route" => route.to_string()).record(duration_secs);
}

/// 记录 server 侧 `ConnectionEstablished` 事件处理耗时（无 label；不含 transport 握手）。
/// 见 LOAD_GATE_EXECUTION_SPEC §7。
pub fn record_connection_established_handling(duration_secs: f64) {
    metrics::histogram!(HISTOGRAM_CONN_ESTABLISHED_HANDLING).record(duration_secs);
}

/// 记录 server 侧认证（AuthorizationRequest 处理）耗时（无 label）。见 LOAD_GATE_EXECUTION_SPEC §7。
pub fn record_authenticate_duration(duration_secs: f64) {
    metrics::histogram!(HISTOGRAM_AUTHENTICATE_DURATION).record(duration_secs);
}

/// 计时守卫：drop 时把耗时记入给定 record 函数。用于包裹含多处 early-return 的路径
/// （如 ConnectMessageHandler：任何 `?`/return 都会在 drop 时统一计时）。
pub struct DurationRecorder {
    start: std::time::Instant,
    record: fn(f64),
}

impl DurationRecorder {
    pub fn new(record: fn(f64)) -> Self {
        Self {
            start: std::time::Instant::now(),
            record,
        }
    }
}

impl Drop for DurationRecorder {
    fn drop(&mut self) {
        (self.record)(self.start.elapsed().as_secs_f64());
    }
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

// ---------------------------------------------------------------------------
// CONNECTION_LIFECYCLE_SPEC 观测埋点
// ---------------------------------------------------------------------------

/// 更新在线用户数（Gauge）。由 60s 轮询 ConnectionManager 写入。
pub fn record_online_users(count: u64) {
    metrics::gauge!(GAUGE_CONN_ONLINE_USERS).set(count as f64);
}

/// 更新在线 session 数（Gauge）。由 60s 轮询 ConnectionManager 写入。
pub fn record_online_sessions(count: u64) {
    metrics::gauge!(GAUGE_CONN_ONLINE_SESSIONS).set(count as f64);
}

/// 累计 replaced session 次数 +1（同设备新连接接管时触发）。
pub fn increment_connection_replaced(by: u64) {
    metrics::counter!(COUNTER_CONN_REPLACED).increment(by);
}

/// 累计 Connecting 超时清理次数 +by（spec §5 GC 调用）。
pub fn increment_connecting_timeout(by: u64) {
    metrics::counter!(COUNTER_CONN_CONNECTING_TIMEOUT).increment(by);
}

/// 累计投递尝试次数 +1（spec §6.1 入口调用）。
pub fn increment_delivery_attempt(by: u64) {
    metrics::counter!(COUNTER_DELIVERY_ATTEMPT).increment(by);
}

/// 累计成功投递 session 次数 +by。
pub fn increment_delivery_success_sessions(by: u64) {
    metrics::counter!(COUNTER_DELIVERY_SUCCESS_SESSIONS).increment(by);
}

/// 累计 zero-success 投递尝试 +1（success_count == 0 时调用）。
pub fn increment_delivery_zero_success(by: u64) {
    metrics::counter!(COUNTER_DELIVERY_ZERO_SUCCESS).increment(by);
}

/// 累计离线队列入队次数 +by（实际写 Redis 的调用点）。
pub fn increment_offline_enqueue(by: u64) {
    metrics::counter!(COUNTER_OFFLINE_ENQUEUE).increment(by);
}

/// 累计因 A→B 过滤被丢弃的 session 次数 +by（spec §6.1 二次校验）。
///
/// reason 取值：`not_authenticated` / `superseded` / `missing_in_b`
pub fn increment_delivery_filtered(reason: &'static str, by: u64) {
    metrics::counter!(COUNTER_DELIVERY_FILTERED, "reason" => reason).increment(by);
}

// ---------------------------------------------------------------------------
// P1-00 Observability baseline：entity sync 维度 + 进程内大 map 水位
// （PRIVCHAT_SERVER_REMEDIATION_TASKS P1-00；SERVER_BUSY 已由
//   privchat_handler_rejected_total 覆盖——限流拒绝与 SERVER_BUSY 响应同源）
// ---------------------------------------------------------------------------

/// entity/sync_entities 请求数（按 entity_type 分）
const COUNTER_SYNC_ENTITIES_REQUESTS: &str = "privchat_sync_entities_requests_total";
/// entity/sync_entities 处理时长（按 entity_type 分）
const HISTOGRAM_SYNC_ENTITIES_DURATION: &str = "privchat_sync_entities_duration_seconds";
/// entity/sync_entities 下发 item 总数（按 entity_type 分）。
/// 重连风暴放大的直接观测面：增量正常时 channel 类应接近 0。
const COUNTER_SYNC_ENTITIES_ITEMS: &str = "privchat_sync_entities_items_total";
/// 进程内大 map 当前条目数（label: map）。P1-15 有界化的水位依据。
const GAUGE_MEMORY_MAP_ENTRIES: &str = "privchat_memory_map_entries";
/// room 订阅：当前有订阅者的 channel 数
const GAUGE_ROOM_SUBSCRIBED_CHANNELS: &str = "privchat_room_subscribed_channels";
/// room 订阅：当前 session 订阅总数
const GAUGE_ROOM_SUBSCRIBER_SESSIONS: &str = "privchat_room_subscriber_sessions";

/// 记录一次 entity/sync_entities 请求：次数 + 时长 + 下发 item 数。
pub fn record_sync_entities(entity_type: &str, duration_secs: f64, items: usize) {
    metrics::counter!(COUNTER_SYNC_ENTITIES_REQUESTS, "entity_type" => entity_type.to_string())
        .increment(1);
    metrics::histogram!(HISTOGRAM_SYNC_ENTITIES_DURATION, "entity_type" => entity_type.to_string())
        .record(duration_secs);
    metrics::counter!(COUNTER_SYNC_ENTITIES_ITEMS, "entity_type" => entity_type.to_string())
        .increment(items as u64);
}

/// 更新某个进程内大 map 的条目数（Gauge，由 60s 轮询写入）。
pub fn record_memory_map_entries(map: &'static str, entries: usize) {
    metrics::gauge!(GAUGE_MEMORY_MAP_ENTRIES, "map" => map).set(entries as f64);
}

/// 更新 room 订阅规模（Gauge，由 60s 轮询写入）。
pub fn record_room_subscribers(channels: usize, sessions: usize) {
    metrics::gauge!(GAUGE_ROOM_SUBSCRIBED_CHANNELS).set(channels as f64);
    metrics::gauge!(GAUGE_ROOM_SUBSCRIBER_SESSIONS).set(sessions as f64);
}
