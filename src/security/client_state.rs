use serde::{Deserialize, Serialize};
/// 客户端安全状态机
///
/// 这是 IM 服务器对抗恶意行为的核心机制
/// 参考 Telegram / Discord 的分层处罚策略
use std::time::{Duration, Instant};

/// 客户端状态（状态机）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientState {
    /// 正常状态
    Normal,

    /// 降速状态
    /// - 消息延迟处理（200ms-1s）
    /// - RPC 响应变慢
    /// - 用于轻度超限
    Throttled,

    /// 写禁用状态
    /// - 允许读操作（sync、pull、ack）
    /// - 禁止写操作（send、post、upload）
    /// - 用于持续超限
    WriteDisabled,

    /// 影子封禁（Shadow Ban）
    /// - 所有操作返回成功
    /// - 但实际不生效（silent drop）
    /// - 消息不会投递
    /// - RPC 不会执行
    /// - 用于明显恶意行为
    ShadowBanned,

    /// 强制断开
    /// - 主动断开连接
    /// - 拒绝重连（短期）
    /// - 用于协议攻击、DoS
    Disconnected,
}

impl ClientState {
    /// 是否允许发送消息
    pub fn can_send_message(&self) -> bool {
        matches!(
            self,
            ClientState::Normal | ClientState::Throttled | ClientState::ShadowBanned
        )
    }

    /// 是否允许执行写 RPC
    pub fn can_write_rpc(&self) -> bool {
        matches!(
            self,
            ClientState::Normal | ClientState::Throttled | ClientState::ShadowBanned
        )
    }

    /// 是否允许执行读 RPC
    pub fn can_read_rpc(&self) -> bool {
        !matches!(self, ClientState::Disconnected)
    }

    /// 是否需要 silent drop（假装成功但不执行）
    pub fn should_silent_drop(&self) -> bool {
        matches!(self, ClientState::ShadowBanned)
    }

    /// 获取延迟时间（用于 Throttled 状态）
    pub fn get_throttle_delay(&self) -> Option<Duration> {
        match self {
            ClientState::Throttled => Some(Duration::from_millis(500)),
            _ => None,
        }
    }
}

/// 状态升级条件
#[derive(Debug, Clone)]
pub struct StateTransition {
    pub from: ClientState,
    pub to: ClientState,
    pub reason: ViolationType,
    pub duration: Option<Duration>,
}

/// 违规类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViolationType {
    /// 速率超限
    RateLimit,
    /// 认证失败
    AuthFailed,
    /// 畸形请求
    MalformedRequest,
    /// 可疑行为
    SuspiciousActivity,
    /// 协议攻击
    ProtocolAttack,
    /// 群发骚扰
    Spam,
    /// Fan-out 滥用（大群高频发消息）
    FanoutAbuse,
}

/// 用户/设备信任度模型
#[derive(Debug, Clone)]
pub struct TrustScore {
    /// 用户 ID
    pub user_id: u64,
    /// 设备 ID
    pub device_id: String,

    /// 信任分（0-1000）
    /// - 新用户：100
    /// - 正常用户：500
    /// - 活跃用户：800+
    /// - 违规用户：< 100
    pub score: u32,

    /// 账号年龄（天）
    pub account_age_days: u32,

    /// 违规历史记录
    pub violations: Vec<ViolationRecord>,

    /// 最后更新时间
    pub last_updated: Instant,
}

impl TrustScore {
    pub fn new(user_id: u64, device_id: String) -> Self {
        Self {
            user_id,
            device_id,
            score: 100, // 新用户默认低信任
            account_age_days: 0,
            violations: Vec::new(),
            last_updated: Instant::now(),
        }
    }

    /// 根据信任分计算容忍度
    /// 高信任用户可以有更高的限流阈值
    pub fn get_rate_multiplier(&self) -> f64 {
        match self.score {
            0..=100 => 0.5,    // 低信任：减半
            101..=300 => 0.8,  // 中低信任
            301..=600 => 1.0,  // 正常
            601..=800 => 1.5,  // 高信任：增加 50%
            801..=1000 => 2.0, // 超高信任：翻倍
            _ => 1.0,
        }
    }

    /// 记录违规行为（降低信任分）
    pub fn record_violation(&mut self, violation_type: ViolationType) {
        let penalty = match violation_type {
            ViolationType::RateLimit => 5,
            ViolationType::AuthFailed => 10,
            ViolationType::MalformedRequest => 20,
            ViolationType::SuspiciousActivity => 30,
            ViolationType::ProtocolAttack => 100,
            ViolationType::Spam => 50,
            ViolationType::FanoutAbuse => 40,
        };

        self.score = self.score.saturating_sub(penalty);
        self.violations.push(ViolationRecord {
            violation_type,
            timestamp: Instant::now(),
            penalty,
        });
        self.last_updated = Instant::now();

        // 只保留最近 100 条记录
        if self.violations.len() > 100 {
            self.violations.drain(0..self.violations.len() - 100);
        }
    }

    /// 奖励正常行为（恢复信任分）
    pub fn reward_good_behavior(&mut self, points: u32) {
        self.score = (self.score + points).min(1000);
        self.last_updated = Instant::now();
    }

    /// 每日自动恢复（时间会洗白）
    pub fn daily_recovery(&mut self) {
        if self.score < 500 {
            self.score = (self.score + 10).min(500);
        }
    }
}

#[derive(Debug, Clone)]
pub struct ViolationRecord {
    pub violation_type: ViolationType,
    pub timestamp: Instant,
    pub penalty: u32,
}

/// 客户端状态管理器
#[derive(Debug)]
pub struct ClientStateManager {
    /// 当前状态
    state: ClientState,
    /// 信任分
    trust_score: TrustScore,
    /// 状态转换历史
    state_history: Vec<StateTransition>,
    /// 状态过期时间（用于临时状态）
    state_expiry: Option<Instant>,
}

impl ClientStateManager {
    pub fn new(user_id: u64, device_id: String) -> Self {
        Self {
            state: ClientState::Normal,
            trust_score: TrustScore::new(user_id, device_id),
            state_history: Vec::new(),
            state_expiry: None,
        }
    }

    /// 获取当前状态
    pub fn current_state(&self) -> ClientState {
        // 检查是否过期
        if let Some(expiry) = self.state_expiry {
            if Instant::now() >= expiry {
                return ClientState::Normal; // 自动恢复
            }
        }
        self.state
    }

    /// 转换状态
    pub fn transition_to(
        &mut self,
        new_state: ClientState,
        reason: ViolationType,
        duration: Option<Duration>,
    ) {
        let old_state = self.state;
        self.state = new_state;

        // 记录状态转换
        self.state_history.push(StateTransition {
            from: old_state,
            to: new_state,
            reason,
            duration,
        });

        // 设置过期时间（如果是临时状态）
        if let Some(duration) = duration {
            self.state_expiry = Some(Instant::now() + duration);
        } else {
            self.state_expiry = None;
        }

        // 记录违规（降低信任分）
        self.trust_score.record_violation(reason);
    }

    /// 根据违规类型自动决定状态升级
    pub fn auto_escalate(&mut self, violation: ViolationType) {
        let current = self.current_state();

        match (current, violation) {
            // 正常 -> 降速
            (ClientState::Normal, ViolationType::RateLimit) => {
                self.transition_to(
                    ClientState::Throttled,
                    violation,
                    Some(Duration::from_secs(60)),
                );
            }

            // 降速 -> 写禁用
            (ClientState::Throttled, ViolationType::RateLimit) => {
                self.transition_to(
                    ClientState::WriteDisabled,
                    violation,
                    Some(Duration::from_secs(300)),
                );
            }

            // 持续违规 -> 影子封禁
            (ClientState::WriteDisabled, _) => {
                self.transition_to(
                    ClientState::ShadowBanned,
                    violation,
                    Some(Duration::from_secs(3600)),
                );
            }

            // 协议攻击 -> 直接断开
            (_, ViolationType::ProtocolAttack) => {
                self.transition_to(
                    ClientState::Disconnected,
                    violation,
                    Some(Duration::from_secs(1800)),
                );
            }

            // Spam -> 影子封禁
            (_, ViolationType::Spam) => {
                self.transition_to(
                    ClientState::ShadowBanned,
                    violation,
                    Some(Duration::from_secs(7200)),
                );
            }

            _ => {
                // 默认：降速
                self.transition_to(
                    ClientState::Throttled,
                    violation,
                    Some(Duration::from_secs(30)),
                );
            }
        }
    }

    /// 获取信任分
    pub fn trust_score(&self) -> &TrustScore {
        &self.trust_score
    }

    /// 获取信任分（可变）
    pub fn trust_score_mut(&mut self) -> &mut TrustScore {
        &mut self.trust_score
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_machine() {
        let mut manager = ClientStateManager::new(1001, "device_1".to_string());

        // 初始状态
        assert_eq!(manager.current_state(), ClientState::Normal);

        // 轻度违规 -> 降速
        manager.auto_escalate(ViolationType::RateLimit);
        assert_eq!(manager.current_state(), ClientState::Throttled);

        // 持续违规 -> 写禁用
        manager.auto_escalate(ViolationType::RateLimit);
        assert_eq!(manager.current_state(), ClientState::WriteDisabled);

        // 再次违规 -> 影子封禁
        manager.auto_escalate(ViolationType::RateLimit);
        assert_eq!(manager.current_state(), ClientState::ShadowBanned);
    }

    #[test]
    fn test_trust_score() {
        let mut trust = TrustScore::new(1001, "device_1".to_string());

        // 初始分数
        assert_eq!(trust.score, 100);

        // 记录违规
        trust.record_violation(ViolationType::RateLimit);
        assert!(trust.score < 100);

        // 奖励
        trust.reward_good_behavior(50);
        assert!(trust.score > 95);
    }
}
