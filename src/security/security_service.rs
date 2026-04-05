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

/// 中央安全服务
///
/// 整合所有安全组件：
/// - 客户端状态管理
/// - 多维度限流
/// - IP 防护
/// - Shadow Ban 执行
///
/// ## 分阶段启用策略
///
/// 通过 SecurityMode 控制能力启用程度，避免早期过度处罚：
///
/// - ObserveOnly: 只记录，不处罚（早期推荐）
/// - EnforceLight: 轻量限流，不启用高级特性
/// - EnforceFull: 全部特性启用
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use super::client_state::{ClientState, ClientStateManager, ViolationType};
use super::rate_limiter::{FanoutCostCalculator, MultiDimensionRateLimiter, RateLimitConfig};

/// 安全模式（分阶段启用）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityMode {
    /// 观察模式（只记录，不处罚）
    ///
    /// 适用场景：
    /// - 项目早期（< 1万用户）
    /// - 协议不稳定
    /// - 客户端有 bug
    ///
    /// 启用的能力：
    /// - ✅ 记录所有违规行为
    /// - ✅ 打点和日志
    /// - ✅ IP 连接频率限制（非常宽松）
    /// - ❌ 不限流
    /// - ❌ 不扣信任分
    /// - ❌ 不改变状态
    ObserveOnly,

    /// 轻量执行模式
    ///
    /// 适用场景：
    /// - 有 1~5 万 DAU
    /// - 协议稳定
    /// - 开始有恶意行为
    ///
    /// 启用的能力：
    /// - ✅ 基础限流（per-user 全局）
    /// - ✅ IP 连接限制（正常严格度）
    /// - ✅ 状态机：Normal / Throttled（2级）
    /// - ✅ 轻量信任度模型
    /// - ❌ 不启用 Shadow Ban
    /// - ❌ 不启用 WriteDisabled
    /// - ❌ 不计算 fan-out cost
    EnforceLight,

    /// 完全执行模式
    ///
    /// 适用场景：
    /// - 稳定期（> 5 万 DAU）
    /// - 有明确的恶意攻击
    /// - 数据充足，cost 模型准确
    ///
    /// 启用的能力：
    /// - ✅ 全部限流（多维度）
    /// - ✅ 完整状态机（4级）
    /// - ✅ Shadow Ban
    /// - ✅ Fan-out 成本计算
    /// - ✅ 信任度自动恢复
    /// - ✅ 自动处罚升级
    EnforceFull,
}

/// IP 保护记录
#[derive(Debug, Clone)]
struct IpProtectionRecord {
    /// 最近违规时间戳
    recent_violations: Vec<Instant>,
    /// 临时封禁到期时间
    temp_ban_until: Option<Instant>,
    /// 是否永久封禁
    permanent_ban: bool,
}

impl IpProtectionRecord {
    fn new() -> Self {
        Self {
            recent_violations: Vec::new(),
            temp_ban_until: None,
            permanent_ban: false,
        }
    }

    fn is_banned(&self) -> bool {
        if self.permanent_ban {
            return true;
        }

        if let Some(until) = self.temp_ban_until {
            if Instant::now() < until {
                return true;
            }
        }

        false
    }

    fn add_violation(&mut self) {
        let now = Instant::now();
        self.recent_violations.push(now);

        // 只保留最近1小时的记录
        self.recent_violations
            .retain(|t| now.duration_since(*t) < Duration::from_secs(3600));
    }

    fn should_temp_ban(&self) -> bool {
        // 5分钟内10次违规
        let five_min_ago = Instant::now() - Duration::from_secs(300);
        let recent_count = self
            .recent_violations
            .iter()
            .filter(|t| **t > five_min_ago)
            .count();

        recent_count >= 10
    }

    fn should_permanent_ban(&self) -> bool {
        // 1小时内50次违规
        let one_hour_ago = Instant::now() - Duration::from_secs(3600);
        let recent_count = self
            .recent_violations
            .iter()
            .filter(|t| **t > one_hour_ago)
            .count();

        recent_count >= 50
    }
}

/// 安全检查结果
#[derive(Debug, Clone)]
pub struct SecurityCheckResult {
    /// 是否允许
    pub allowed: bool,
    /// 客户端当前状态
    pub client_state: ClientState,
    /// 是否需要 silent drop
    pub should_silent_drop: bool,
    /// 延迟时间（用于 throttle）
    pub throttle_delay: Option<Duration>,
    /// 拒绝原因
    pub reason: Option<String>,
}

impl SecurityCheckResult {
    pub fn allow(client_state: ClientState) -> Self {
        Self {
            allowed: true,
            client_state,
            should_silent_drop: false,
            throttle_delay: client_state.get_throttle_delay(),
            reason: None,
        }
    }

    pub fn deny(reason: String) -> Self {
        Self {
            allowed: false,
            client_state: ClientState::Disconnected,
            should_silent_drop: false,
            throttle_delay: None,
            reason: Some(reason),
        }
    }

    pub fn shadow_ban() -> Self {
        Self {
            allowed: true,
            client_state: ClientState::ShadowBanned,
            should_silent_drop: true,
            throttle_delay: None,
            reason: Some("Shadow banned".to_string()),
        }
    }
}

/// 中央安全服务
pub struct SecurityService {
    /// 用户/设备状态管理器
    client_states: Arc<RwLock<HashMap<(u64, String), ClientStateManager>>>,

    /// 多维度限流器
    rate_limiter: Arc<MultiDimensionRateLimiter>,

    /// IP 保护记录
    ip_protections: Arc<RwLock<HashMap<String, IpProtectionRecord>>>,

    /// 配置
    config: SecurityConfig,
}

#[derive(Debug, Clone)]
pub struct SecurityConfig {
    /// 安全模式（控制启用程度）
    pub mode: SecurityMode,

    /// 是否启用 Shadow Ban（仅在 EnforceFull 模式下生效）
    pub enable_shadow_ban: bool,

    /// 是否启用 IP 封禁
    pub enable_ip_ban: bool,

    /// 限流配置
    pub rate_limit: RateLimitConfig,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            // 默认观察模式（早期推荐）
            mode: SecurityMode::ObserveOnly,
            enable_shadow_ban: true,
            enable_ip_ban: true,
            rate_limit: RateLimitConfig::default(),
        }
    }
}

impl SecurityConfig {
    /// 早期阶段配置（推荐）
    pub fn early_stage() -> Self {
        Self {
            mode: SecurityMode::ObserveOnly,
            enable_shadow_ban: false,
            enable_ip_ban: true,
            rate_limit: RateLimitConfig {
                user_tokens_per_second: 100.0, // 宽松
                user_burst_capacity: 200.0,
                channel_messages_per_second: 10.0, // 宽松
                channel_burst_capacity: 30.0,
                ip_connections_per_second: 10.0, // 宽松
                ip_burst_capacity: 20.0,
            },
        }
    }

    /// 生产环境配置
    pub fn production() -> Self {
        Self {
            mode: SecurityMode::EnforceLight,
            enable_shadow_ban: false, // 还不启用
            enable_ip_ban: true,
            rate_limit: RateLimitConfig::default(),
        }
    }

    /// 严格模式配置
    pub fn strict() -> Self {
        Self {
            mode: SecurityMode::EnforceFull,
            enable_shadow_ban: true,
            enable_ip_ban: true,
            rate_limit: RateLimitConfig {
                user_tokens_per_second: 30.0, // 严格
                user_burst_capacity: 60.0,
                channel_messages_per_second: 2.0, // 严格
                channel_burst_capacity: 5.0,
                ip_connections_per_second: 3.0, // 严格
                ip_burst_capacity: 5.0,
            },
        }
    }
}

impl SecurityService {
    pub fn new(config: SecurityConfig) -> Self {
        Self {
            client_states: Arc::new(RwLock::new(HashMap::new())),
            rate_limiter: Arc::new(MultiDimensionRateLimiter::new(config.rate_limit.clone())),
            ip_protections: Arc::new(RwLock::new(HashMap::new())),
            config,
        }
    }

    /// 连接层检查（IP 级别）
    pub async fn check_connection(&self, ip: &str) -> SecurityCheckResult {
        // ObserveOnly 模式：只记录，不限制
        if self.config.mode == SecurityMode::ObserveOnly {
            debug!("🔍 [ObserveOnly] 连接检查: IP {}", ip);
            return SecurityCheckResult::allow(ClientState::Normal);
        }

        if !self.config.enable_ip_ban {
            return SecurityCheckResult::allow(ClientState::Normal);
        }

        // 1. 检查 IP 是否被封禁
        let ip_protections = self.ip_protections.read().await;
        if let Some(record) = ip_protections.get(ip) {
            if record.is_banned() {
                warn!("🚫 IP {} 被封禁（模式: {:?}）", ip, self.config.mode);
                return SecurityCheckResult::deny(format!("IP {} is banned", ip));
            }
        }
        drop(ip_protections);

        // 2. 连接速率限制
        if !self.rate_limiter.check_ip_connection(ip) {
            // EnforceLight: 只记录，不封禁
            if self.config.mode == SecurityMode::EnforceLight {
                warn!("⚠️ [EnforceLight] IP {} 连接超限（只记录）", ip);
                return SecurityCheckResult::allow(ClientState::Throttled);
            }

            // EnforceFull: 记录 + 封禁
            self.record_ip_violation(ip).await;
            warn!("❌ [EnforceFull] IP {} 连接超限（拒绝）", ip);
            return SecurityCheckResult::deny(format!("IP {} connection rate limit exceeded", ip));
        }

        SecurityCheckResult::allow(ClientState::Normal)
    }

    /// 消息发送检查
    pub async fn check_send_message(
        &self,
        user_id: u64,
        device_id: &str,
        channel_id: u64,
        recipient_count: usize,
        message_size: usize,
        is_media: bool,
    ) -> SecurityCheckResult {
        // ObserveOnly 模式：只记录，完全放行
        if self.config.mode == SecurityMode::ObserveOnly {
            debug!(
                "🔍 [ObserveOnly] 消息发送: user={}, conv={}, recipients={}, size={}",
                user_id, channel_id, recipient_count, message_size
            );
            return SecurityCheckResult::allow(ClientState::Normal);
        }

        // 1. 获取或创建客户端状态管理器
        let mut client_states = self.client_states.write().await;
        let state_manager = client_states
            .entry((user_id, device_id.to_string()))
            .or_insert_with(|| ClientStateManager::new(user_id, device_id.to_string()));

        let current_state = state_manager.current_state();

        // 2. EnforceLight: 只检查基础状态，不启用 WriteDisabled 和 ShadowBan
        if self.config.mode == SecurityMode::EnforceLight {
            // 只允许 Normal 和 Throttled
            if current_state == ClientState::WriteDisabled
                || current_state == ClientState::ShadowBanned
                || current_state == ClientState::Disconnected
            {
                warn!(
                    "⚠️ [EnforceLight] 用户 {} 状态异常 {:?}（降级为 Throttled）",
                    user_id, current_state
                );
                // 降级为 Throttled
                return SecurityCheckResult::allow(ClientState::Throttled);
            }
        }

        // 3. EnforceFull: 检查完整状态
        if self.config.mode == SecurityMode::EnforceFull {
            if !current_state.can_send_message() {
                if current_state.should_silent_drop() && self.config.enable_shadow_ban {
                    warn!("🚫 [EnforceFull] 用户 {} 处于 Shadow Ban", user_id);
                    return SecurityCheckResult::shadow_ban();
                }
                warn!("❌ [EnforceFull] 用户 {} 写操作被禁用", user_id);
                return SecurityCheckResult::deny("Write operations disabled".to_string());
            }
        }

        // 4. 计算消息成本（EnforceFull 模式才考虑 fan-out）
        let message_cost = if self.config.mode == SecurityMode::EnforceFull {
            FanoutCostCalculator::calculate_message_cost(recipient_count, message_size, is_media)
        } else {
            1 // EnforceLight 不计算 fan-out
        };

        // 5. 限流检查
        let trust_score = state_manager.trust_score();
        let (allowed, violation) = self.rate_limiter.check_and_consume(
            user_id,
            "messages.send",
            Some(channel_id),
            trust_score,
        );

        if !allowed {
            if let Some(violation_type) = violation {
                // EnforceLight: 记录违规，但不自动升级状态
                if self.config.mode == SecurityMode::EnforceLight {
                    warn!(
                        "⚠️ [EnforceLight] 用户 {} 消息超限（只记录，不升级状态）",
                        user_id
                    );
                    state_manager
                        .trust_score_mut()
                        .record_violation(violation_type);
                    return SecurityCheckResult::deny("Rate limit exceeded".to_string());
                }

                // EnforceFull: 自动升级状态
                state_manager.auto_escalate(violation_type);
                let new_state = state_manager.current_state();
                warn!(
                    "❌ [EnforceFull] 用户 {} 消息超限，状态升级到 {:?}",
                    user_id, new_state
                );

                if new_state.should_silent_drop() && self.config.enable_shadow_ban {
                    return SecurityCheckResult::shadow_ban();
                }

                return SecurityCheckResult::deny("Rate limit exceeded".to_string());
            }
        }

        // 6. Fan-out 滥用检测（仅 EnforceFull）
        if self.config.mode == SecurityMode::EnforceFull
            && recipient_count > 100
            && message_cost > 10
        {
            state_manager.auto_escalate(ViolationType::FanoutAbuse);
            warn!(
                "🚫 [EnforceFull] 用户 {} 触发 fan-out 滥用（群: {}人）",
                user_id, recipient_count
            );
            if self.config.enable_shadow_ban {
                return SecurityCheckResult::shadow_ban();
            }
        }

        SecurityCheckResult::allow(current_state)
    }

    /// RPC 调用检查
    pub async fn check_rpc(
        &self,
        user_id: u64,
        device_id: &str,
        rpc_method: &str,
        channel_id: Option<u64>,
    ) -> SecurityCheckResult {
        // ObserveOnly 模式：只记录
        if self.config.mode == SecurityMode::ObserveOnly {
            debug!(
                "🔍 [ObserveOnly] RPC: user={}, method={}",
                user_id, rpc_method
            );
            return SecurityCheckResult::allow(ClientState::Normal);
        }

        // 1. 获取或创建客户端状态管理器
        let mut client_states = self.client_states.write().await;
        let state_manager = client_states
            .entry((user_id, device_id.to_string()))
            .or_insert_with(|| ClientStateManager::new(user_id, device_id.to_string()));

        let current_state = state_manager.current_state();

        // 2. EnforceLight: 简化状态检查
        if self.config.mode == SecurityMode::EnforceLight {
            // 只检查 Throttled（允许通过但延迟）
            if current_state == ClientState::Throttled {
                return SecurityCheckResult::allow(ClientState::Throttled);
            }
        }

        // 3. EnforceFull: 完整状态检查
        if self.config.mode == SecurityMode::EnforceFull {
            let is_write_rpc = rpc_method.starts_with("messages.")
                || rpc_method.starts_with("channels.create")
                || rpc_method.starts_with("channels.invite");

            if is_write_rpc && !current_state.can_write_rpc() {
                if current_state.should_silent_drop() && self.config.enable_shadow_ban {
                    warn!(
                        "🚫 [EnforceFull] 用户 {} RPC {} 被 Shadow Ban",
                        user_id, rpc_method
                    );
                    return SecurityCheckResult::shadow_ban();
                }
                warn!("❌ [EnforceFull] 用户 {} 写 RPC 被禁用", user_id);
                return SecurityCheckResult::deny("Write RPC disabled".to_string());
            }

            if !current_state.can_read_rpc() {
                warn!("❌ [EnforceFull] 用户 {} 已断开连接", user_id);
                return SecurityCheckResult::deny("Disconnected".to_string());
            }
        }

        // 4. 限流检查
        let trust_score = state_manager.trust_score();
        let (allowed, violation) =
            self.rate_limiter
                .check_and_consume(user_id, rpc_method, channel_id, trust_score);

        if !allowed {
            if let Some(violation_type) = violation {
                // EnforceLight: 只记录
                if self.config.mode == SecurityMode::EnforceLight {
                    warn!(
                        "⚠️ [EnforceLight] 用户 {} RPC {} 超限（只记录）",
                        user_id, rpc_method
                    );
                    state_manager
                        .trust_score_mut()
                        .record_violation(violation_type);
                    return SecurityCheckResult::deny("Rate limit exceeded".to_string());
                }

                // EnforceFull: 自动升级
                state_manager.auto_escalate(violation_type);
                let new_state = state_manager.current_state();
                warn!(
                    "❌ [EnforceFull] 用户 {} RPC {} 超限，状态升级到 {:?}",
                    user_id, rpc_method, new_state
                );

                if new_state.should_silent_drop() && self.config.enable_shadow_ban {
                    return SecurityCheckResult::shadow_ban();
                }
            }
            return SecurityCheckResult::deny("Rate limit exceeded".to_string());
        }

        SecurityCheckResult::allow(current_state)
    }

    /// 认证失败记录
    pub async fn record_auth_failure(&self, user_id: u64, device_id: &str, ip: &str) {
        let mut client_states = self.client_states.write().await;
        let state_manager = client_states
            .entry((user_id, device_id.to_string()))
            .or_insert_with(|| ClientStateManager::new(user_id, device_id.to_string()));

        state_manager.auto_escalate(ViolationType::AuthFailed);

        // 同时记录 IP 违规
        self.record_ip_violation(ip).await;
    }

    /// 记录 IP 违规
    async fn record_ip_violation(&self, ip: &str) {
        let mut ip_protections = self.ip_protections.write().await;
        let record = ip_protections
            .entry(ip.to_string())
            .or_insert_with(IpProtectionRecord::new);

        record.add_violation();

        // 检查是否需要封禁
        if record.should_permanent_ban() {
            record.permanent_ban = true;
            error!("🚫 IP {} 已被永久封禁（严重违规）", ip);
        } else if record.should_temp_ban() {
            record.temp_ban_until = Some(Instant::now() + Duration::from_secs(1800)); // 30分钟
            warn!("⚠️ IP {} 已被临时封禁 30分钟", ip);
        }
    }

    /// 奖励良好行为（恢复信任分）
    pub async fn reward_good_behavior(&self, user_id: u64, device_id: &str) {
        let mut client_states = self.client_states.write().await;
        if let Some(state_manager) = client_states.get_mut(&(user_id, device_id.to_string())) {
            state_manager.trust_score_mut().reward_good_behavior(1);
        }
    }

    /// 获取用户状态（用于监控和调试）
    pub async fn get_user_state(&self, user_id: u64, device_id: &str) -> Option<ClientState> {
        let client_states = self.client_states.read().await;
        client_states
            .get(&(user_id, device_id.to_string()))
            .map(|m| m.current_state())
    }

    /// 获取用户信任分
    pub async fn get_trust_score(&self, user_id: u64, device_id: &str) -> Option<u32> {
        let client_states = self.client_states.read().await;
        client_states
            .get(&(user_id, device_id.to_string()))
            .map(|m| m.trust_score().score)
    }

    /// 手动封禁用户（管理员操作）
    pub async fn manual_ban(
        &self,
        user_id: u64,
        device_id: &str,
        reason: ViolationType,
        duration: Option<Duration>,
    ) {
        let mut client_states = self.client_states.write().await;
        let state_manager = client_states
            .entry((user_id, device_id.to_string()))
            .or_insert_with(|| ClientStateManager::new(user_id, device_id.to_string()));

        state_manager.transition_to(ClientState::ShadowBanned, reason, duration);
        warn!(
            "管理员封禁用户 {} (设备: {}), 原因: {:?}",
            user_id, device_id, reason
        );
    }

    /// 手动解封用户
    pub async fn manual_unban(&self, user_id: u64, device_id: &str) {
        let mut client_states = self.client_states.write().await;
        if let Some(state_manager) = client_states.get_mut(&(user_id, device_id.to_string())) {
            state_manager.transition_to(
                ClientState::Normal,
                ViolationType::RateLimit, // 占位符
                None,
            );
            info!("管理员解封用户 {} (设备: {})", user_id, device_id);
        }
    }

    // =====================================================
    // 管理 API 方法
    // =====================================================

    /// 获取所有处于 ShadowBanned 状态的用户/设备列表（管理 API）
    pub async fn list_shadow_banned(&self) -> Vec<(u64, String, ClientState, Option<u32>)> {
        let client_states = self.client_states.read().await;
        client_states
            .iter()
            .filter(|(_, mgr)| mgr.current_state() == ClientState::ShadowBanned)
            .map(|((user_id, device_id), mgr)| {
                (
                    *user_id,
                    device_id.clone(),
                    mgr.current_state(),
                    Some(mgr.trust_score().score),
                )
            })
            .collect()
    }

    /// 获取用户所有设备的安全状态（管理 API）
    pub async fn get_user_device_states(
        &self,
        user_id: u64,
        device_ids: &[String],
    ) -> Vec<(String, Option<ClientState>, Option<u32>)> {
        let client_states = self.client_states.read().await;
        device_ids
            .iter()
            .map(|device_id| {
                let key = (user_id, device_id.clone());
                match client_states.get(&key) {
                    Some(mgr) => (
                        device_id.clone(),
                        Some(mgr.current_state()),
                        Some(mgr.trust_score().score),
                    ),
                    None => (device_id.clone(), None, None),
                }
            })
            .collect()
    }

    /// 解除用户所有设备的封禁（管理 API）
    ///
    /// 返回受影响的设备数
    pub async fn unban_all_user_devices(&self, user_id: u64, device_ids: &[String]) -> usize {
        let mut client_states = self.client_states.write().await;
        let mut affected = 0;
        for device_id in device_ids {
            if let Some(state_manager) = client_states.get_mut(&(user_id, device_id.clone())) {
                state_manager.transition_to(
                    ClientState::Normal,
                    ViolationType::RateLimit, // 占位符
                    None,
                );
                affected += 1;
            }
        }
        if affected > 0 {
            info!("管理员批量解封用户 {} ({} 个设备)", user_id, affected);
        }
        affected
    }

    /// 定期清理过期数据（后台任务）
    pub async fn cleanup_expired_data(&self) {
        let client_states = self.client_states.write().await;
        let mut ip_protections = self.ip_protections.write().await;

        // 清理过期的 IP 临时封禁
        ip_protections.retain(|_, record| {
            if let Some(until) = record.temp_ban_until {
                if Instant::now() >= until {
                    return false; // 移除过期的临时封禁
                }
            }
            !record.permanent_ban || !record.recent_violations.is_empty()
        });

        info!(
            "安全服务清理完成：保留 {} 个客户端状态，{} 个 IP 记录",
            client_states.len(),
            ip_protections.len()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_security_service() {
        let config = SecurityConfig::default();
        let service = SecurityService::new(config);

        // 正常消息发送
        let result = service
            .check_send_message(1001, "device_1", 2001, 5, 100, false)
            .await;
        assert!(result.allowed);

        // 大群高频发送（应触发 fan-out 检测）
        let result = service
            .check_send_message(1001, "device_1", 2001, 500, 100, false)
            .await;
        // 第一次可能允许，但多次后会被限制
    }

    #[tokio::test]
    async fn test_ip_protection() {
        let config = SecurityConfig::default();
        let service = SecurityService::new(config);

        // 正常连接
        let result = service.check_connection("192.168.1.1").await;
        assert!(result.allowed);

        // 高频连接（模拟攻击）
        for _ in 0..20 {
            service.record_ip_violation("192.168.1.100").await;
        }

        // 应该被封禁
        let result = service.check_connection("192.168.1.100").await;
        // 临时封禁后应该被拒绝
    }
}
