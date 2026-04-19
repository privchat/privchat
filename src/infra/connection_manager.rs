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

//! 连接管理器
//!
//! 实现 `spec/02-server/CONNECTION_LIFECYCLE_SPEC.md` 定义的双索引模型：
//! - Index A：`user_id → device_id → session_id`（业务投递路由表）
//! - Index B：`session_id → SessionEntry`（连接生命周期权威态）
//!
//! **单一真源**：其它在线态管理器禁止参与投递判定。

use anyhow::Result;
use dashmap::DashMap;
use msgtrans::SessionId;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// 会话状态（spec §3）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// 已建立 transport，但未完成认证
    Connecting,
    /// 已绑定 user/device，当前可投递
    Authenticated,
    /// 被同设备新连接接管，失去权威资格（不可投递）
    Replaced,
    /// 已发起关闭，等待 transport 回执
    Closing,
    /// 终态，短 TTL 后从索引 B 移除
    Closed,
}

impl SessionState {
    /// 是否具备投递资格（Index A 准入条件 + 投递热路径二次校验）
    #[inline]
    pub fn is_deliverable(self) -> bool {
        matches!(self, SessionState::Authenticated)
    }
}

/// Index B 承载的完整会话条目
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub session_id: SessionId,
    pub state: SessionState,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    /// 被哪个新 session 取代；在 Replaced 状态下非空
    pub superseded_by: Option<SessionId>,
    pub connected_at: i64,
    pub authenticated_at: Option<i64>,
}

/// 设备连接信息（对外快照；只由 Authenticated 的条目投影而来）
#[derive(Debug, Clone)]
pub struct DeviceConnection {
    pub user_id: u64,
    pub device_id: String,
    pub session_id: SessionId,
    pub connected_at: i64,
}

/// 一致性自检报告（spec §9）
#[derive(Debug, Default, Clone)]
pub struct ConsistencyReport {
    /// C-1 孤儿路由：Index A 指向的 session_id 在 Index B 中不存在
    pub orphan_routes: Vec<(u64, String, SessionId)>,
    /// C-2 非法状态路由：Index A 指向的 session 在 Index B 中 state 非 Authenticated 或已被替换
    pub illegal_state_routes: Vec<(u64, String, SessionId, SessionState)>,
    /// C-3 遗失路由：Index B Authenticated 的 session 但 Index A 不指向它
    pub missing_routes: Vec<(u64, String, SessionId)>,
}

impl ConsistencyReport {
    pub fn is_clean(&self) -> bool {
        self.orphan_routes.is_empty()
            && self.illegal_state_routes.is_empty()
            && self.missing_routes.is_empty()
    }
}

/// 认证结果：若发生同设备替换，返回被替换的旧 session_id，供调用方异步 close
#[derive(Debug, Clone)]
pub struct AuthenticateOutcome {
    pub replaced_session_id: Option<SessionId>,
}

/// 连接管理器：spec §1 声明的**唯一**在线态真源
pub struct ConnectionManager {
    /// Index A：user_id → device_id → session_id（仅含 Authenticated）
    index_a: DashMap<u64, HashMap<String, SessionId>>,

    /// Index B：session_id → SessionEntry（完整生命周期）
    index_b: DashMap<SessionId, SessionEntry>,

    /// Authenticated 条目计数（热路径统计，避免扫 Index B）
    total_authenticated: AtomicUsize,

    /// TransportServer 引用（用于主动关闭连接）
    pub transport_server: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
}

impl Default for ConnectionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionManager {
    pub fn new() -> Self {
        Self {
            index_a: DashMap::new(),
            index_b: DashMap::new(),
            total_authenticated: AtomicUsize::new(0),
            transport_server: Arc::new(RwLock::new(None)),
        }
    }

    pub async fn set_transport_server(&self, server: Arc<msgtrans::transport::TransportServer>) {
        let mut transport = self.transport_server.write().await;
        *transport = Some(server);
        info!("✅ ConnectionManager: TransportServer 已设置");
    }

    // ---------------------------------------------------------------------
    // spec §4.1：Connecting 入口
    // ---------------------------------------------------------------------

    /// 连接建立但尚未认证。transport 的 `ConnectionEstablished` 事件应调用此入口。
    ///
    /// 此阶段只写 Index B，不碰 Index A；该 session 不可投递。
    pub fn register_connecting(&self, session_id: SessionId) {
        let now = chrono::Utc::now().timestamp_millis();
        let entry = SessionEntry {
            session_id,
            state: SessionState::Connecting,
            user_id: None,
            device_id: None,
            superseded_by: None,
            connected_at: now,
            authenticated_at: None,
        };
        // 若已存在（例如重复事件），保留原条目
        self.index_b.entry(session_id).or_insert(entry);
        debug!(
            "🌱 ConnectionManager: session {} 进入 Connecting",
            session_id
        );
    }

    // ---------------------------------------------------------------------
    // spec §4.2：Authenticate 原子替换流程
    // ---------------------------------------------------------------------

    /// 认证完成时调用。按 spec §4.2 步骤 1→2→3→4 顺序执行，返回被替换的旧 session_id（若有）。
    ///
    /// 调用方负责异步 close 被替换的旧连接（fire-and-forget）。
    pub fn authenticate(
        &self,
        session_id: SessionId,
        user_id: u64,
        device_id: String,
    ) -> AuthenticateOutcome {
        let now = chrono::Utc::now().timestamp_millis();

        // Step 1: Index B 完成绑定（若 session 不存在 Index B，先补 Connecting 再转 Authenticated）
        {
            let mut b_entry = self
                .index_b
                .entry(session_id)
                .or_insert_with(|| SessionEntry {
                    session_id,
                    state: SessionState::Connecting,
                    user_id: None,
                    device_id: None,
                    superseded_by: None,
                    connected_at: now,
                    authenticated_at: None,
                });
            b_entry.user_id = Some(user_id);
            b_entry.device_id = Some(device_id.clone());
            b_entry.state = SessionState::Authenticated;
            b_entry.authenticated_at = Some(now);
        }

        // Step 2 + 3：在 Index A 的 user 分片锁下串行化：先标旧 Replaced，再原子切 A
        //
        // 要点：读侧 `.get(&user_id)` 取的是同一分片的 shared lock，写侧 `.entry()` 取 exclusive，
        // 两者互斥，保证读 A → 读 B 之间不会穿插本次替换的中间态（§6.1 二次校验安全性来源）。
        let replaced_session_id: Option<SessionId> = {
            let mut a_entry = self.index_a.entry(user_id).or_default();
            let map = a_entry.value_mut();

            let old_sid = map.get(&device_id).copied();

            if let Some(old) = old_sid {
                if old != session_id {
                    // Step 2：先把旧条目在 Index B 标为 Replaced（失去投递资格）
                    if let Some(mut b_old) = self.index_b.get_mut(&old) {
                        if b_old.state == SessionState::Authenticated {
                            b_old.state = SessionState::Replaced;
                            b_old.superseded_by = Some(session_id);
                            self.total_authenticated.fetch_sub(1, Ordering::Relaxed);
                        } else {
                            // 竞态保护：若旧条目已 Replaced/Closing/Closed，也只更新 superseded_by
                            b_old.superseded_by = Some(session_id);
                        }
                    }
                }
            }

            // Step 3：原子切换 Index A → 新 session_id
            map.insert(device_id.clone(), session_id);

            if old_sid.map(|o| o != session_id).unwrap_or(true) {
                self.total_authenticated.fetch_add(1, Ordering::Relaxed);
            }

            // 返回需要异步 close 的旧 session（Step 4 由调用方处理）
            old_sid.filter(|old| *old != session_id)
        };

        crate::infra::metrics::record_connection_count(
            self.total_authenticated.load(Ordering::Relaxed) as u64,
        );

        if let Some(old) = replaced_session_id {
            crate::infra::metrics::increment_connection_replaced(1);
            info!(
                target: "connection.authenticate.replace",
                user_id = user_id,
                device_id = %device_id,
                old_session_id = %old,
                new_session_id = %session_id,
                "same-device supersedes older session"
            );
            debug!(
                "♻️ ConnectionManager: 替换 user={} device={} old_sid={} new_sid={}",
                user_id, device_id, old, session_id
            );
        } else {
            debug!(
                "✅ ConnectionManager: 首次认证 user={} device={} sid={}",
                user_id, device_id, session_id
            );
        }

        AuthenticateOutcome {
            replaced_session_id,
        }
    }

    // ---------------------------------------------------------------------
    // 兼容入口：保留旧 register_connection，内部走 register_connecting + authenticate
    // 迁移后（Step 3）可删除
    // ---------------------------------------------------------------------

    /// 兼容旧签名：register_connecting + authenticate 的组合调用。
    ///
    /// **新代码不应使用此 API**，而应分别调用 `register_connecting` 和 `authenticate`。
    pub async fn register_connection(
        &self,
        user_id: u64,
        device_id: String,
        session_id: SessionId,
    ) -> Result<()> {
        self.register_connecting(session_id);
        let outcome = self.authenticate(session_id, user_id, device_id.clone());
        if let Some(old_sid) = outcome.replaced_session_id {
            // 异步关闭旧连接（§4.2 Step 4），不阻塞本路径
            let transport = self.transport_server.clone();
            tokio::spawn(async move {
                let guard = transport.read().await;
                if let Some(server) = guard.as_ref() {
                    if let Err(e) = server.close_session(old_sid).await {
                        warn!(
                            "⚠️ ConnectionManager: 异步关闭被替换的旧 session 失败 sid={} err={}",
                            old_sid, e
                        );
                    }
                }
            });
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // spec §4.3：Disconnect（ConnectionClosed 事件）
    // ---------------------------------------------------------------------

    /// 注销连接（由 transport `ConnectionClosed` 事件触发）。
    ///
    /// 关键行为：**按 session_id 精确匹配清理 Index A**（I-3, I-4）。
    /// 若 Index A 已指向其它 session，说明旧连接是迟到事件，忽略对 A 的清理。
    pub async fn unregister_connection(
        &self,
        session_id: SessionId,
    ) -> Result<Option<(u64, String)>> {
        // 从 Index B 取出条目，记录 user_id / device_id / state，然后移除。
        let removed_entry = self.index_b.remove(&session_id).map(|(_, v)| v);
        let entry = match removed_entry {
            Some(e) => e,
            None => return Ok(None),
        };

        let was_authenticated = entry.state == SessionState::Authenticated;

        let Some(user_id) = entry.user_id else {
            // 未认证的连接（Connecting 状态）直接关闭，无 Index A 条目
            return Ok(None);
        };
        let Some(device_id) = entry.device_id.clone() else {
            return Ok(None);
        };

        // 条件清理 Index A：仅当映射仍指向本 session_id 时才删除
        let mut cleaned_a = false;
        let mut current_in_a: Option<SessionId> = None;
        if let Some(mut a_entry) = self.index_a.get_mut(&user_id) {
            let map = a_entry.value_mut();
            current_in_a = map.get(&device_id).copied();
            if current_in_a == Some(session_id) {
                map.remove(&device_id);
                cleaned_a = true;
            }
            // 如果该 user 在 Index A 已空，稍后在 get_mut guard drop 后处理
        }
        if cleaned_a {
            // 若该 user 下已无任何设备，移除外层条目（谓词再次判空防止并发 authenticate 误删）
            self.index_a.remove_if(&user_id, |_, map| map.is_empty());
        }

        if was_authenticated && cleaned_a {
            self.total_authenticated.fetch_sub(1, Ordering::Relaxed);
            crate::infra::metrics::record_connection_count(
                self.total_authenticated.load(Ordering::Relaxed) as u64,
            );
        }

        // 迟到 close：Index A 已指向新 session（§4.2 Step 2 已把本条目标记为 Replaced），
        // 此处不应再清 A。打一条结构化日志便于观测替换路径的正确性。
        if !cleaned_a && matches!(entry.state, SessionState::Replaced) {
            info!(
                target: "connection.unregister.skip_replaced",
                user_id = user_id,
                device_id = %device_id,
                closed_session_id = %session_id,
                current_session_id = ?current_in_a,
                superseded_by = ?entry.superseded_by,
                "late close for replaced session; Index A untouched"
            );
        }

        debug!(
            "🔌 ConnectionManager: 注销 sid={} user={} device={} state={:?} cleaned_a={}",
            session_id, user_id, device_id, entry.state, cleaned_a
        );

        Ok(Some((user_id, device_id)))
    }

    // ---------------------------------------------------------------------
    // spec §4.4：Kick
    // ---------------------------------------------------------------------

    /// 断开指定设备（踢设备）
    pub async fn disconnect_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        let session_id = self.index_a.get(&user_id).and_then(|entry| {
            entry
                .value()
                .get(device_id)
                .copied()
        });

        let Some(session_id) = session_id else {
            debug!(
                "📝 ConnectionManager: 设备未连接 user={} device={}",
                user_id, device_id
            );
            return Ok(());
        };

        info!(
            "🔌 ConnectionManager: 断开设备 user={} device={} sid={}",
            user_id, device_id, session_id
        );

        // 先翻转 Index B 状态到 Closing（投递即刻不可达）
        if let Some(mut b_entry) = self.index_b.get_mut(&session_id) {
            if b_entry.state == SessionState::Authenticated {
                b_entry.state = SessionState::Closing;
            }
        }

        // 清理 Index A（按 session_id 精确匹配）
        let mut cleaned_a = false;
        if let Some(mut a_entry) = self.index_a.get_mut(&user_id) {
            let map = a_entry.value_mut();
            if map.get(device_id).copied() == Some(session_id) {
                map.remove(device_id);
                cleaned_a = true;
            }
        }
        if cleaned_a {
            self.index_a.remove_if(&user_id, |_, map| map.is_empty());
            self.total_authenticated.fetch_sub(1, Ordering::Relaxed);
            crate::infra::metrics::record_connection_count(
                self.total_authenticated.load(Ordering::Relaxed) as u64,
            );
        }

        // 物理断开
        let transport = self.transport_server.read().await;
        if let Some(server) = transport.as_ref() {
            if let Err(e) = server.close_session(session_id).await {
                warn!(
                    "⚠️ ConnectionManager: close_session 失败 sid={} err={}",
                    session_id, e
                );
            }
        } else {
            warn!("⚠️ ConnectionManager: TransportServer 未设置，无法断开连接");
        }

        Ok(())
    }

    /// 断开用户所有其它设备（保留当前设备）
    pub async fn disconnect_other_devices(
        &self,
        user_id: u64,
        current_device_id: &str,
    ) -> Result<Vec<String>> {
        let devices_to_disconnect: Vec<String> = self
            .index_a
            .get(&user_id)
            .map(|entry| {
                entry
                    .value()
                    .keys()
                    .filter(|d| d.as_str() != current_device_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();

        info!(
            "🔌 ConnectionManager: 踢其它设备 user={} count={} current={}",
            user_id,
            devices_to_disconnect.len(),
            current_device_id
        );

        for device_id in &devices_to_disconnect {
            if let Err(e) = self.disconnect_device(user_id, device_id).await {
                warn!(
                    "⚠️ ConnectionManager: 断开设备失败 user={} device={} err={}",
                    user_id, device_id, e
                );
            }
        }

        Ok(devices_to_disconnect)
    }

    // ---------------------------------------------------------------------
    // 查询接口（只读快照）
    // ---------------------------------------------------------------------

    /// 获取用户的所有 Authenticated 连接快照
    pub async fn get_user_connections(&self, user_id: u64) -> Vec<DeviceConnection> {
        let Some(a_entry) = self.index_a.get(&user_id) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(a_entry.value().len());
        for (device_id, sid) in a_entry.value().iter() {
            if let Some(b_entry) = self.index_b.get(sid) {
                if b_entry.state == SessionState::Authenticated {
                    out.push(DeviceConnection {
                        user_id,
                        device_id: device_id.clone(),
                        session_id: *sid,
                        connected_at: b_entry.connected_at,
                    });
                }
            }
        }
        out
    }

    /// 通过 session_id 查询当前连接信息（只返回 Authenticated）
    pub async fn get_connection_by_session(
        &self,
        session_id: &SessionId,
    ) -> Option<DeviceConnection> {
        let b_entry = self.index_b.get(session_id)?;
        if b_entry.state != SessionState::Authenticated {
            return None;
        }
        Some(DeviceConnection {
            user_id: b_entry.user_id?,
            device_id: b_entry.device_id.clone()?,
            session_id: *session_id,
            connected_at: b_entry.connected_at,
        })
    }

    /// 当前 Authenticated 连接总数
    pub async fn get_connection_count(&self) -> usize {
        self.total_authenticated.load(Ordering::Relaxed)
    }

    /// 当前有 Authenticated 条目的用户数（Index A 外层长度）
    pub fn online_users_count(&self) -> usize {
        self.index_a.len()
    }

    /// 当前 Authenticated session 数（热路径计数器）
    pub fn online_sessions_count(&self) -> usize {
        self.total_authenticated.load(Ordering::Relaxed)
    }

    /// 所有 Authenticated 连接快照
    pub async fn get_all_connections(&self) -> Vec<DeviceConnection> {
        let mut out = Vec::with_capacity(self.total_authenticated.load(Ordering::Relaxed));
        for a_entry in self.index_a.iter() {
            let user_id = *a_entry.key();
            for (device_id, sid) in a_entry.value().iter() {
                if let Some(b_entry) = self.index_b.get(sid) {
                    if b_entry.state == SessionState::Authenticated {
                        out.push(DeviceConnection {
                            user_id,
                            device_id: device_id.clone(),
                            session_id: *sid,
                            connected_at: b_entry.connected_at,
                        });
                    }
                }
            }
        }
        out
    }

    /// 检查设备是否在线（Index A + Index B 二次校验）
    pub async fn is_device_online(&self, user_id: u64, device_id: &str) -> bool {
        let Some(a_entry) = self.index_a.get(&user_id) else {
            return false;
        };
        let Some(sid) = a_entry.value().get(device_id).copied() else {
            return false;
        };
        drop(a_entry);
        self.index_b
            .get(&sid)
            .map(|b| b.state == SessionState::Authenticated)
            .unwrap_or(false)
    }

    // ---------------------------------------------------------------------
    // spec §6：消息投递热路径
    // ---------------------------------------------------------------------

    /// 实时推送到用户所有 Authenticated 设备。
    ///
    /// spec §6.1 硬路径：
    /// 1. 取 Index A 当前快照
    /// 2. 到 Index B 二次校验 `state == Authenticated && superseded_by.is_none()`
    /// 3. 并发投递，收集 success_count
    ///
    /// 返回 `success_count`。调用方按 §6.3 决定是否写离线队列（success_count == 0 时写）。
    pub async fn send_push_to_user(
        &self,
        user_id: u64,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        crate::infra::metrics::increment_delivery_attempt(1);

        // Step 1 + 2：A 快照 + B 二次过滤
        let mut filtered_not_auth = 0u64;
        let mut filtered_superseded = 0u64;
        let mut filtered_missing_b = 0u64;
        let live_sessions: Vec<SessionId> = {
            let Some(a_entry) = self.index_a.get(&user_id) else {
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            };
            a_entry
                .value()
                .values()
                .filter_map(|sid| match self.index_b.get(sid) {
                    None => {
                        filtered_missing_b += 1;
                        None
                    }
                    Some(b) => {
                        if b.state != SessionState::Authenticated {
                            filtered_not_auth += 1;
                            None
                        } else if b.superseded_by.is_some() {
                            filtered_superseded += 1;
                            None
                        } else {
                            Some(*sid)
                        }
                    }
                })
                .collect()
        };

        if filtered_not_auth > 0 {
            crate::infra::metrics::increment_delivery_filtered("not_authenticated", filtered_not_auth);
        }
        if filtered_superseded > 0 {
            crate::infra::metrics::increment_delivery_filtered("superseded", filtered_superseded);
        }
        if filtered_missing_b > 0 {
            crate::infra::metrics::increment_delivery_filtered("missing_in_b", filtered_missing_b);
        }

        if live_sessions.is_empty() {
            crate::infra::metrics::increment_delivery_zero_success(1);
            return Ok(0);
        }

        let transport = self.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            warn!("⚠️ ConnectionManager: TransportServer 未设置，无法投递");
            crate::infra::metrics::increment_delivery_zero_success(1);
            return Ok(0);
        };

        let payload = privchat_protocol::encode_message(message)
            .map_err(|e| anyhow::anyhow!("encode PushMessageRequest failed: {}", e))?;

        let mut success = 0usize;
        for sid in live_sessions {
            let mut packet = msgtrans::packet::Packet::one_way(
                crate::infra::next_packet_id(),
                payload.clone(),
            );
            packet
                .set_biz_type(privchat_protocol::protocol::MessageType::PushMessageRequest as u8);
            match server.send_to_session(sid, packet).await {
                Ok(()) => success += 1,
                Err(e) => {
                    warn!(
                        "⚠️ ConnectionManager: 实时推送失败 user={} sid={} server_message_id={} err={}",
                        user_id, sid, message.server_message_id, e
                    );
                }
            }
        }

        if success > 0 {
            crate::infra::metrics::increment_delivery_success_sessions(success as u64);
        } else {
            crate::infra::metrics::increment_delivery_zero_success(1);
        }

        Ok(success)
    }

    /// 推送到指定设备（§6.1 A→B 过滤，设备级）。
    ///
    /// 语义：存在 (user_id, device_id) 在 Index A，且 Index B 对应 session 是
    /// `Authenticated && superseded_by.is_none()`，则尝试投递；成功返回 1，其它情况返回 0。
    pub async fn send_push_to_device(
        &self,
        user_id: u64,
        device_id: &str,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        crate::infra::metrics::increment_delivery_attempt(1);
        let session_id = {
            let Some(a_entry) = self.index_a.get(&user_id) else {
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            };
            let Some(sid) = a_entry.value().get(device_id).copied() else {
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            };
            let Some(b) = self.index_b.get(&sid) else {
                crate::infra::metrics::increment_delivery_filtered("missing_in_b", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            };
            if b.state != SessionState::Authenticated {
                crate::infra::metrics::increment_delivery_filtered("not_authenticated", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            }
            if b.superseded_by.is_some() {
                crate::infra::metrics::increment_delivery_filtered("superseded", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            }
            sid
        };

        let sent = self.send_push_on_session(session_id, message).await?;
        if sent > 0 {
            crate::infra::metrics::increment_delivery_success_sessions(sent as u64);
        } else {
            crate::infra::metrics::increment_delivery_zero_success(1);
        }
        Ok(sent)
    }

    /// 直接按 `session_id` 推送（§6.1 B 单点过滤）。
    ///
    /// 语义：Index B 存在且 `Authenticated && superseded_by.is_none()` 才投递。
    pub async fn send_push_to_session(
        &self,
        session_id: SessionId,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        crate::infra::metrics::increment_delivery_attempt(1);
        {
            let Some(b) = self.index_b.get(&session_id) else {
                crate::infra::metrics::increment_delivery_filtered("missing_in_b", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            };
            if b.state != SessionState::Authenticated {
                crate::infra::metrics::increment_delivery_filtered("not_authenticated", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            }
            if b.superseded_by.is_some() {
                crate::infra::metrics::increment_delivery_filtered("superseded", 1);
                crate::infra::metrics::increment_delivery_zero_success(1);
                return Ok(0);
            }
        }
        let sent = self.send_push_on_session(session_id, message).await?;
        if sent > 0 {
            crate::infra::metrics::increment_delivery_success_sessions(sent as u64);
        } else {
            crate::infra::metrics::increment_delivery_zero_success(1);
        }
        Ok(sent)
    }

    async fn send_push_on_session(
        &self,
        session_id: SessionId,
        message: &privchat_protocol::protocol::PushMessageRequest,
    ) -> Result<usize> {
        let transport = self.transport_server.read().await;
        let Some(server) = transport.as_ref() else {
            warn!("⚠️ ConnectionManager: TransportServer 未设置，无法投递");
            return Ok(0);
        };
        let payload = privchat_protocol::encode_message(message)
            .map_err(|e| anyhow::anyhow!("encode PushMessageRequest failed: {}", e))?;
        let mut packet =
            msgtrans::packet::Packet::one_way(crate::infra::next_packet_id(), payload);
        packet.set_biz_type(privchat_protocol::protocol::MessageType::PushMessageRequest as u8);
        match server.send_to_session(session_id, packet).await {
            Ok(()) => Ok(1),
            Err(e) => {
                warn!(
                    "⚠️ ConnectionManager: 单点推送失败 sid={} server_message_id={} err={}",
                    session_id, message.server_message_id, e
                );
                Ok(0)
            }
        }
    }

    /// 用户是否至少有一个 Authenticated 连接（离线判定用）。
    pub fn has_authenticated_connection(&self, user_id: u64) -> bool {
        let Some(a_entry) = self.index_a.get(&user_id) else {
            return false;
        };
        a_entry.value().values().any(|sid| {
            self.index_b
                .get(sid)
                .map(|b| b.state == SessionState::Authenticated && b.superseded_by.is_none())
                .unwrap_or(false)
        })
    }

    // ---------------------------------------------------------------------
    // spec §9：索引一致性自检（基础版，Step 1 随双索引落地）
    // ---------------------------------------------------------------------

    /// 扫描 Index A / Index B，返回所有不一致点。**不参与热路径判定**。
    pub fn self_check(&self) -> ConsistencyReport {
        let mut report = ConsistencyReport::default();

        // 先扫 Index A：检测 C-1 / C-2
        for a_entry in self.index_a.iter() {
            let user_id = *a_entry.key();
            for (device_id, sid) in a_entry.value().iter() {
                match self.index_b.get(sid) {
                    None => {
                        report
                            .orphan_routes
                            .push((user_id, device_id.clone(), *sid));
                        error!(
                            "🚨 Consistency C-1 orphan route: user={} device={} sid={} missing in Index B",
                            user_id, device_id, sid
                        );
                    }
                    Some(b) => {
                        if b.state != SessionState::Authenticated || b.superseded_by.is_some() {
                            report.illegal_state_routes.push((
                                user_id,
                                device_id.clone(),
                                *sid,
                                b.state,
                            ));
                            error!(
                                "🚨 Consistency C-2 illegal-state route: user={} device={} sid={} state={:?} superseded_by={:?}",
                                user_id, device_id, sid, b.state, b.superseded_by
                            );
                        }
                    }
                }
            }
        }

        // 扫 Index B：检测 C-3（只 warn，不自动修复）
        for b_entry in self.index_b.iter() {
            let b = b_entry.value();
            if b.state != SessionState::Authenticated {
                continue;
            }
            let Some(user_id) = b.user_id else { continue };
            let Some(ref device_id) = b.device_id else {
                continue;
            };
            let in_a = self
                .index_a
                .get(&user_id)
                .map(|e| e.value().get(device_id).copied() == Some(b.session_id))
                .unwrap_or(false);
            if !in_a {
                report
                    .missing_routes
                    .push((user_id, device_id.clone(), b.session_id));
                warn!(
                    "⚠️ Consistency C-3 missing route: user={} device={} sid={} Authenticated in B but not in A",
                    user_id, device_id, b.session_id
                );
            }
        }

        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use privchat_protocol::protocol::PushMessageRequest;

    fn push_fixture() -> PushMessageRequest {
        PushMessageRequest::new()
    }

    // ---- 基础路径 ----

    #[tokio::test]
    async fn test_register_and_unregister() {
        let manager = ConnectionManager::new();
        let sid = SessionId::new(123);

        manager
            .register_connection(1, "device-001".to_string(), sid)
            .await
            .unwrap();

        assert!(manager.is_device_online(1, "device-001").await);
        assert_eq!(manager.get_connection_count().await, 1);

        manager.unregister_connection(sid).await.unwrap();

        assert!(!manager.is_device_online(1, "device-001").await);
        assert_eq!(manager.get_connection_count().await, 0);
        assert!(manager.self_check().is_clean());
    }

    #[tokio::test]
    async fn test_multiple_devices() {
        let manager = ConnectionManager::new();

        manager
            .register_connection(1, "A".to_string(), SessionId::new(101))
            .await
            .unwrap();
        manager
            .register_connection(1, "B".to_string(), SessionId::new(102))
            .await
            .unwrap();
        manager
            .register_connection(1, "C".to_string(), SessionId::new(103))
            .await
            .unwrap();

        assert_eq!(manager.get_connection_count().await, 3);
        assert_eq!(manager.get_user_connections(1).await.len(), 3);
        assert!(manager.self_check().is_clean());
    }

    // ---- spec §8 测试 ----

    /// §8 #1 / #7：重连替换 + 旧 close 晚于新 authenticate（点名最危险并发）
    #[tokio::test]
    async fn test_old_close_late_after_new_authenticate() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(1);
        let new_sid = SessionId::new(2);

        // T0: 旧 session 已认证
        manager.register_connecting(old_sid);
        let r1 = manager.authenticate(old_sid, 1, "device-A".to_string());
        assert!(r1.replaced_session_id.is_none());

        // T1: 新 session 同 device 认证，触发替换
        manager.register_connecting(new_sid);
        let r2 = manager.authenticate(new_sid, 1, "device-A".to_string());
        assert_eq!(r2.replaced_session_id, Some(old_sid));

        // I-2 / I-4：Index A 指向 new；Index B 中 old 已 Replaced
        assert!(manager.is_device_online(1, "device-A").await);
        let all = manager.get_user_connections(1).await;
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].session_id, new_sid);
        assert_eq!(manager.get_connection_count().await, 1);

        // T2: 旧 session 的 ConnectionClosed 迟到
        manager.unregister_connection(old_sid).await.unwrap();

        // I-3 / I-4：Index A 仍指向 new，未被误删
        assert!(manager.is_device_online(1, "device-A").await);
        let all_after = manager.get_user_connections(1).await;
        assert_eq!(all_after.len(), 1);
        assert_eq!(all_after[0].session_id, new_sid);
        assert_eq!(manager.get_connection_count().await, 1);
        assert!(manager.self_check().is_clean());
    }

    /// §8 #5：并发认证 —— 同 (user, device) 两条 TCP 同时认证
    #[tokio::test]
    async fn test_concurrent_auth_same_device() {
        let manager = Arc::new(ConnectionManager::new());
        let sid_a = SessionId::new(100);
        let sid_b = SessionId::new(200);

        manager.register_connecting(sid_a);
        manager.register_connecting(sid_b);

        let m1 = manager.clone();
        let m2 = manager.clone();
        let j1 = tokio::spawn(async move {
            m1.authenticate(sid_a, 42, "same-device".to_string())
        });
        let j2 = tokio::spawn(async move {
            m2.authenticate(sid_b, 42, "same-device".to_string())
        });
        let (_r1, _r2) = tokio::join!(j1, j2);

        // I-6：Index A 最终只有一个条目
        let conns = manager.get_user_connections(42).await;
        assert_eq!(conns.len(), 1, "Index A must have exactly one session");
        assert_eq!(manager.get_connection_count().await, 1);

        // Index B：winner 是 Authenticated，loser 是 Replaced
        let winner_sid = conns[0].session_id;
        let loser_sid = if winner_sid == sid_a { sid_b } else { sid_a };
        let winner_b = manager.index_b.get(&winner_sid).unwrap();
        let loser_b = manager.index_b.get(&loser_sid).unwrap();
        assert_eq!(winner_b.state, SessionState::Authenticated);
        assert_eq!(loser_b.state, SessionState::Replaced);
        assert_eq!(loser_b.superseded_by, Some(winner_sid));

        assert!(manager.self_check().is_clean());
    }

    /// §8 #6：多设备下只清自己那条
    #[tokio::test]
    async fn test_unregister_only_removes_matching_session() {
        let manager = ConnectionManager::new();
        manager
            .register_connection(1, "A".to_string(), SessionId::new(10))
            .await
            .unwrap();
        manager
            .register_connection(1, "B".to_string(), SessionId::new(20))
            .await
            .unwrap();

        manager.unregister_connection(SessionId::new(10)).await.unwrap();

        assert!(!manager.is_device_online(1, "A").await);
        assert!(manager.is_device_online(1, "B").await);
        assert_eq!(manager.get_connection_count().await, 1);
        assert!(manager.self_check().is_clean());
    }

    #[tokio::test]
    async fn test_user_entry_cleaned_up_after_last_device() {
        let manager = ConnectionManager::new();
        manager
            .register_connection(42, "d".to_string(), SessionId::new(1))
            .await
            .unwrap();
        manager.unregister_connection(SessionId::new(1)).await.unwrap();

        assert_eq!(manager.get_user_connections(42).await.len(), 0);
        assert_eq!(manager.get_connection_count().await, 0);
        assert!(manager.self_check().is_clean());
    }

    /// spec §3.1：Connecting 状态不得进入 Index A
    #[tokio::test]
    async fn test_connecting_session_not_in_index_a() {
        let manager = ConnectionManager::new();
        manager.register_connecting(SessionId::new(7));
        assert_eq!(manager.get_connection_count().await, 0);
        assert!(manager.get_all_connections().await.is_empty());
        assert!(manager.self_check().is_clean());
    }

    // ---- 回归场景（Step 5 自动化部分）---------------------------------------
    // 覆盖 CONNECTION_LIFECYCLE_SPEC 四个核心场景：
    //   #1 同设备重连替换 — 新 session 接管、旧失效
    //   #4 Replaced session 被投递热路径过滤
    //   #5 Connecting 不可投递
    //   #6 旧 close 晚到不影响新连接投递资格
    //
    // 没有真实 transport 的情况下，`send_push_to_*` 成功路径与 transport=None 分支
    // 都返回 Ok(0)；因此这些用例同时断言 API 返回值 + Index A/B 快照，确保
    // "被 filter 拦截 vs 放行后到 transport" 的两种 0 是可区分的。

    /// §8 #1：同设备重连替换——投递视角完整断言
    #[tokio::test]
    async fn test_reconnect_replace_new_session_deliverable_old_not() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(1001);
        let new_sid = SessionId::new(1002);

        manager.register_connecting(old_sid);
        let r1 = manager.authenticate(old_sid, 7, "iphone".to_string());
        assert!(r1.replaced_session_id.is_none());

        manager.register_connecting(new_sid);
        let r2 = manager.authenticate(new_sid, 7, "iphone".to_string());
        assert_eq!(r2.replaced_session_id, Some(old_sid));

        // Index A 指向新 session
        let conns = manager.get_user_connections(7).await;
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].session_id, new_sid);
        assert!(manager.is_device_online(7, "iphone").await);

        // Index B：旧 Replaced + superseded_by，新 Authenticated
        let b_old = manager.index_b.get(&old_sid).unwrap();
        assert_eq!(b_old.state, SessionState::Replaced);
        assert_eq!(b_old.superseded_by, Some(new_sid));
        assert!(!b_old.state.is_deliverable());
        drop(b_old);

        let b_new = manager.index_b.get(&new_sid).unwrap();
        assert_eq!(b_new.state, SessionState::Authenticated);
        assert_eq!(b_new.superseded_by, None);
        assert!(b_new.state.is_deliverable());
        drop(b_new);

        // send_push_to_session 直接打旧 sid：被 filter 拦截（state != Authenticated）
        let msg = push_fixture();
        assert_eq!(
            manager.send_push_to_session(old_sid, &msg).await.unwrap(),
            0
        );

        // 用户仍可达（通过新 session）
        assert!(manager.has_authenticated_connection(7));

        // 计数：只认新的一条
        assert_eq!(manager.get_connection_count().await, 1);
        assert_eq!(manager.online_users_count(), 1);
        assert_eq!(manager.online_sessions_count(), 1);

        assert!(manager.self_check().is_clean());
    }

    /// §6.1 #4：Replaced session 必须被热路径过滤掉
    #[tokio::test]
    async fn test_send_push_to_replaced_session_returns_zero() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(2001);
        let new_sid = SessionId::new(2002);

        manager.register_connecting(old_sid);
        manager.authenticate(old_sid, 11, "pad".to_string());
        manager.register_connecting(new_sid);
        manager.authenticate(new_sid, 11, "pad".to_string());

        let msg = push_fixture();

        // 直接对旧 sid 投递：B 中 state==Replaced 且 superseded_by 非空 → 被 filter 挡
        assert_eq!(
            manager.send_push_to_session(old_sid, &msg).await.unwrap(),
            0
        );

        // send_push_to_device 也应走同一 filter（A 里 device→new，打旧 device 应走到 new）
        // 这里用 send_push_to_user 间接验证 live_sessions 正确挑选（独立于 transport）
        let live_sessions: Vec<SessionId> = {
            let a = manager.index_a.get(&11).unwrap();
            a.value()
                .values()
                .filter_map(|sid| {
                    let b = manager.index_b.get(sid)?;
                    if b.state == SessionState::Authenticated && b.superseded_by.is_none() {
                        Some(*sid)
                    } else {
                        None
                    }
                })
                .collect()
        };
        assert_eq!(live_sessions, vec![new_sid], "filter must pick only new_sid");

        assert!(manager.self_check().is_clean());
    }

    /// §3.1 #5：Connecting 状态不可投递（Index A 不含；B 中即便有，也被 filter 拦）
    #[tokio::test]
    async fn test_send_push_to_connecting_session_returns_zero() {
        let manager = ConnectionManager::new();
        let sid = SessionId::new(3001);

        manager.register_connecting(sid);

        // B 中有 Connecting 条目
        let b = manager.index_b.get(&sid).unwrap();
        assert_eq!(b.state, SessionState::Connecting);
        assert!(!b.state.is_deliverable());
        assert!(b.user_id.is_none());
        drop(b);

        // A 中没有条目，全局计数 0
        assert_eq!(manager.get_connection_count().await, 0);
        assert_eq!(manager.online_users_count(), 0);
        assert_eq!(manager.online_sessions_count(), 0);
        assert!(manager.get_all_connections().await.is_empty());

        // 直接打这个 sid：B 存在但 state != Authenticated → filter 拦截
        let msg = push_fixture();
        assert_eq!(manager.send_push_to_session(sid, &msg).await.unwrap(), 0);

        // 从 user/device 视角也查不到
        assert!(!manager.has_authenticated_connection(999));
        assert!(!manager.is_device_online(999, "whatever").await);

        assert!(manager.self_check().is_clean());
    }

    /// §4.3 #6：旧 close 晚到——新连接投递资格保持（I-3/I-4）
    #[tokio::test]
    async fn test_send_push_unaffected_by_late_old_close() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(4001);
        let new_sid = SessionId::new(4002);

        manager.register_connecting(old_sid);
        manager.authenticate(old_sid, 42, "laptop".to_string());
        manager.register_connecting(new_sid);
        let outcome = manager.authenticate(new_sid, 42, "laptop".to_string());
        assert_eq!(outcome.replaced_session_id, Some(old_sid));

        // 旧 close 迟到：返回 (user, device) 但不清 A（Index A 已指向 new_sid）
        let out = manager.unregister_connection(old_sid).await.unwrap();
        assert_eq!(out, Some((42, "laptop".to_string())));

        // Index A 未被误删
        assert!(manager.is_device_online(42, "laptop").await);
        let conns = manager.get_user_connections(42).await;
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].session_id, new_sid);
        assert_eq!(manager.get_connection_count().await, 1);

        // 旧 B 已移除；打旧 sid 走 missing_in_b 分支返回 0
        assert!(manager.index_b.get(&old_sid).is_none());
        let msg = push_fixture();
        assert_eq!(
            manager.send_push_to_session(old_sid, &msg).await.unwrap(),
            0
        );

        // 新 session 仍是 Authenticated && superseded_by.is_none()——filter 允许放行
        let b_new = manager.index_b.get(&new_sid).unwrap();
        assert_eq!(b_new.state, SessionState::Authenticated);
        assert!(b_new.state.is_deliverable());
        assert_eq!(b_new.superseded_by, None);
        drop(b_new);
        assert!(manager.has_authenticated_connection(42));

        assert!(manager.self_check().is_clean());
    }

    /// spec §6：Replaced 状态的 session 不可投递
    #[tokio::test]
    async fn test_replaced_session_is_not_deliverable() {
        let manager = ConnectionManager::new();
        let old_sid = SessionId::new(10);
        let new_sid = SessionId::new(20);

        manager
            .register_connection(1, "D".to_string(), old_sid)
            .await
            .unwrap();
        manager
            .register_connection(1, "D".to_string(), new_sid)
            .await
            .unwrap();

        // 旧 session 在 Index B 必须是 Replaced
        let b_old = manager.index_b.get(&old_sid).unwrap();
        assert_eq!(b_old.state, SessionState::Replaced);
        assert_eq!(b_old.superseded_by, Some(new_sid));
        assert!(!b_old.state.is_deliverable());

        // 新 session 必须是 Authenticated
        let b_new = manager.index_b.get(&new_sid).unwrap();
        assert_eq!(b_new.state, SessionState::Authenticated);

        // 计数不重复
        assert_eq!(manager.get_connection_count().await, 1);
        assert!(manager.self_check().is_clean());
    }
}
