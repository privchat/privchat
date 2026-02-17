use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// IM JWT Token Claims
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImTokenClaims {
    /// JWT 标准字段 - 签发者
    pub iss: String,
    /// JWT 标准字段 - 主题 (用户ID)
    pub sub: String,
    /// JWT 标准字段 - 受众
    pub aud: String,
    /// JWT 标准字段 - 过期时间 (Unix timestamp)
    pub exp: i64,
    /// JWT 标准字段 - 签发时间
    pub iat: i64,
    /// JWT 标准字段 - JWT ID (用于撤销)
    pub jti: String,

    /// 自定义字段 - 设备ID
    pub device_id: String,
    /// 自定义字段 - 业务系统ID
    pub business_system_id: String,
    /// 自定义字段 - 应用ID
    pub app_id: String,

    /// 会话版本号（用于设备级撤销）
    /// 只在安全事件时递增（登出、改密、被踢等）
    #[serde(default = "default_session_version")]
    pub session_version: i64,
}

fn default_session_version() -> i64 {
    1
}

/// 设备信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 应用ID (ios/android/web/pc)
    pub app_id: String,

    /// 设备名称 (用户可修改，如 "我的 iPhone")
    pub device_name: String,

    /// 设备型号 (如 "iPhone 15 Pro")
    pub device_model: String,

    /// 操作系统 (如 "iOS 17.2")
    pub os_version: String,

    /// APP 版本 (如 "1.0.0")
    pub app_version: String,
}

/// 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DeviceType {
    #[serde(rename = "ios")]
    IOS,
    #[serde(rename = "android")]
    Android,
    #[serde(rename = "macos")]
    MacOS,
    #[serde(rename = "windows")]
    Windows,
    #[serde(rename = "linux")]
    Linux,
    #[serde(rename = "mobile")]
    Mobile,
    #[serde(rename = "desktop")]
    Desktop,
    #[serde(rename = "web")]
    Web,
    #[serde(rename = "unknown")]
    Unknown,
}

impl DeviceType {
    pub fn from_app_id(app_id: &str) -> Self {
        match app_id.to_lowercase().as_str() {
            "ios" => Self::IOS,
            "android" => Self::Android,
            "macos" => Self::MacOS,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            "mobile" => Self::Mobile,
            "desktop" => Self::Desktop,
            "web" => Self::Web,
            _ => Self::Unknown,
        }
    }
}

/// 设备记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// 设备唯一ID (UUID)
    pub device_id: String,

    /// 所属用户ID
    pub user_id: u64,

    /// 业务系统ID (哪个业务系统的用户)
    pub business_system_id: String,

    /// 设备信息
    pub device_info: DeviceInfo,

    /// 设备类型
    pub device_type: DeviceType,

    /// 当前 token 的 JWT ID (用于撤销)
    pub token_jti: String,

    /// 会话版本号（用于设备级撤销）
    #[serde(default = "default_session_version")]
    pub session_version: i64,

    /// 会话状态
    #[serde(default)]
    pub session_state: crate::auth::SessionState,

    /// 被踢时间
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kicked_at: Option<i64>,

    /// 被踢原因
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kicked_reason: Option<String>,

    /// 最后活跃时间
    pub last_active_at: DateTime<Utc>,

    /// 创建时间 (首次登录)
    pub created_at: DateTime<Utc>,

    /// 登录 IP 地址
    pub ip_address: String,
}

/// Token 签发请求
#[derive(Debug, Deserialize)]
pub struct IssueTokenRequest {
    /// 用户ID
    pub user_id: u64,

    /// 业务系统ID
    pub business_system_id: String,

    /// 设备ID（可选，必须是 UUID 格式。如果不提供，服务器会自动生成）
    ///
    /// **重要**：如果提供，客户端在连接 WebSocket 时必须使用相同的 device_id
    pub device_id: Option<String>,

    /// 设备信息
    pub device_info: DeviceInfo,

    /// 自定义 TTL (可选，单位：秒)
    pub ttl: Option<i64>,
}

/// Token 签发响应
#[derive(Debug, Serialize)]
pub struct IssueTokenResponse {
    /// IM token
    pub im_token: String,

    /// 设备ID
    pub device_id: String,

    /// 过期时间（秒）
    pub expires_in: i64,

    /// 过期时间戳
    pub expires_at: DateTime<Utc>,
}

/// 设备列表响应
#[derive(Debug, Serialize)]
pub struct DeviceListResponse {
    pub devices: Vec<DeviceItem>,
    pub total: usize,
}

/// 设备列表项
#[derive(Debug, Serialize)]
pub struct DeviceItem {
    pub device_id: String,
    pub device_name: String,
    pub device_model: String,
    pub app_id: String,
    pub device_type: DeviceType,
    pub last_active_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub ip_address: String,
    pub is_current: bool,
}

/// Service Key 配置
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceKeyConfig {
    pub key: String,
    pub name: String,
}
