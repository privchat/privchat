// =====================================================
// 设备会话状态管理
// =====================================================

use serde::{Deserialize, Serialize};
use sqlx::Type;

/// 设备会话状态（显式状态机）
///
/// 这是一个显式状态机，不是黑名单。
/// 每个状态都有明确的语义和恢复策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[repr(i16)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SessionState {
    /// 活跃状态（正常使用）
    Active = 0,

    /// 被踢状态（被其他设备踢出）
    /// - 可以重新登录
    /// - 会显示"您的账号在其他设备登录"
    Kicked = 1,

    /// 冻结状态（风控或临时封禁）
    /// - 需要通过风控验证才能恢复
    /// - 可能需要二次验证
    Frozen = 2,

    /// 撤销状态（永久不可用）
    /// - 密码修改、账号注销等场景
    /// - 必须重新登录
    Revoked = 3,

    /// 待验证状态（需要二次验证）
    /// - 异地登录、新设备等场景
    /// - 完成验证后恢复为 Active
    PendingVerify = 4,
}

impl SessionState {
    /// 判断状态是否可用（允许连接）
    pub fn is_usable(&self) -> bool {
        matches!(self, SessionState::Active)
    }

    /// 判断是否可以重新激活（重新登录）
    pub fn can_reactivate(&self) -> bool {
        matches!(
            self,
            SessionState::Kicked | SessionState::Frozen | SessionState::PendingVerify
        )
    }

    /// 判断是否需要断开连接
    pub fn should_disconnect(&self) -> bool {
        !self.is_usable()
    }

    /// 获取用户友好的错误消息
    pub fn to_error_message(&self) -> &'static str {
        match self {
            SessionState::Active => "设备正常",
            SessionState::Kicked => "您的账号已在其他设备登录",
            SessionState::Frozen => "设备已被冻结，请联系客服",
            SessionState::Revoked => "设备已被撤销，请重新登录",
            SessionState::PendingVerify => "需要进行安全验证",
        }
    }

    /// 获取状态名称
    pub fn to_string(&self) -> &'static str {
        match self {
            SessionState::Active => "ACTIVE",
            SessionState::Kicked => "KICKED",
            SessionState::Frozen => "FROZEN",
            SessionState::Revoked => "REVOKED",
            SessionState::PendingVerify => "PENDING_VERIFY",
        }
    }
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState::Active
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string())
    }
}

/// 设备会话验证结果
#[derive(Debug, Clone)]
pub enum SessionVerifyResult {
    /// 验证通过
    Valid { session_version: i64 },

    /// 设备不存在
    DeviceNotFound,

    /// 会话状态不可用
    SessionInactive {
        state: SessionState,
        message: String,
    },

    /// Token 版本不匹配（已失效）
    VersionMismatch {
        token_version: i64,
        current_version: i64,
    },
}

impl SessionVerifyResult {
    pub fn is_valid(&self) -> bool {
        matches!(self, SessionVerifyResult::Valid { .. })
    }
}

/// 被踢设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KickedDevice {
    pub device_id: String,
    pub device_name: Option<String>,
    pub device_type: String,
    pub old_version: i64,
    pub new_version: i64,
}

/// 踢设备原因枚举
#[derive(Debug, Clone, Copy)]
pub enum KickReason {
    /// 用户主动踢出其他设备
    UserRequested,

    /// 密码修改
    PasswordChanged,

    /// 账号注销
    AccountDeleted,

    /// 风控触发
    SecurityTrigger,

    /// 管理员操作
    AdminAction,

    /// 设备异常
    AbnormalBehavior,
}

impl KickReason {
    pub fn to_string(&self) -> &'static str {
        match self {
            KickReason::UserRequested => "user_requested",
            KickReason::PasswordChanged => "password_changed",
            KickReason::AccountDeleted => "account_deleted",
            KickReason::SecurityTrigger => "security_trigger",
            KickReason::AdminAction => "admin_action",
            KickReason::AbnormalBehavior => "abnormal_behavior",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_state_usable() {
        assert!(SessionState::Active.is_usable());
        assert!(!SessionState::Kicked.is_usable());
        assert!(!SessionState::Frozen.is_usable());
        assert!(!SessionState::Revoked.is_usable());
        assert!(!SessionState::PendingVerify.is_usable());
    }

    #[test]
    fn test_session_state_reactivate() {
        assert!(!SessionState::Active.can_reactivate());
        assert!(SessionState::Kicked.can_reactivate());
        assert!(SessionState::Frozen.can_reactivate());
        assert!(!SessionState::Revoked.can_reactivate());
        assert!(SessionState::PendingVerify.can_reactivate());
    }

    #[test]
    fn test_session_state_messages() {
        assert_eq!(
            SessionState::Kicked.to_error_message(),
            "您的账号已在其他设备登录"
        );
        assert_eq!(
            SessionState::Revoked.to_error_message(),
            "设备已被撤销，请重新登录"
        );
    }
}
