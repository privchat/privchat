use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// QR 码类型
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QRType {
    /// 用户名片
    User,
    /// 群组邀请
    Group,
    /// 扫码登录
    Auth,
    /// 其他功能
    Feature,
}

impl QRType {
    pub fn as_str(&self) -> &str {
        match self {
            QRType::User => "user",
            QRType::Group => "group",
            QRType::Auth => "auth",
            QRType::Feature => "feature",
        }
    }
    
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(QRType::User),
            "group" => Some(QRType::Group),
            "auth" => Some(QRType::Auth),
            "feature" => Some(QRType::Feature),
            _ => None,
        }
    }
}

/// QR Key 记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QRKeyRecord {
    /// 唯一的 QR Key（随机生成）
    pub qr_key: String,
    
    /// QR 码类型
    pub qr_type: QRType,
    
    /// 目标 ID（user_id 或 group_id）
    pub target_id: String,
    
    /// 创建者 ID
    pub creator_id: String,
    
    /// 创建时间
    pub created_at: DateTime<Utc>,
    
    /// 过期时间（可选）
    pub expire_at: Option<DateTime<Utc>>,
    
    /// 最大使用次数（可选）
    pub max_usage: Option<i32>,
    
    /// 已使用次数
    pub used_count: i32,
    
    /// 是否已撤销
    pub revoked: bool,
    
    /// 扩展信息（如群组 token 等）
    pub metadata: serde_json::Value,
}

/// QR Key 生成选项
#[derive(Debug, Clone, Default)]
pub struct QRKeyOptions {
    /// 过期时间（秒）
    pub expire_seconds: Option<i64>,
    
    /// 最大使用次数
    pub max_usage: Option<i32>,
    
    /// 是否撤销旧的 QR Key（默认 true）
    pub revoke_old: bool,
    
    /// 是否一次性（默认 false）
    pub one_time: bool,
    
    /// 扩展信息
    pub metadata: serde_json::Value,
}

/// 扫描日志
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QRScanLog {
    pub qr_key: String,
    pub scanner_id: String,
    pub scanned_at: DateTime<Utc>,
    pub success: bool,
    pub error: Option<String>,
}

impl QRKeyRecord {
    /// 检查是否过期
    pub fn is_expired(&self) -> bool {
        if let Some(expire_at) = self.expire_at {
            Utc::now() > expire_at
        } else {
            false
        }
    }
    
    /// 检查是否达到使用上限
    pub fn is_usage_exceeded(&self) -> bool {
        if let Some(max_usage) = self.max_usage {
            self.used_count >= max_usage
        } else {
            false
        }
    }
    
    /// 检查是否有效（未撤销、未过期、未超限）
    pub fn is_valid(&self) -> bool {
        !self.revoked && !self.is_expired() && !self.is_usage_exceeded()
    }
    
    /// 生成完整的二维码字符串
    pub fn to_qr_code_string(&self) -> String {
        let base = format!("privchat://{}/get?qrkey={}", self.qr_type.as_str(), self.qr_key);
        
        // 如果有额外的 token（群组邀请时需要）
        if let Some(token) = self.metadata.get("token").and_then(|t| t.as_str()) {
            format!("{}&token={}", base, token)
        } else {
            base
        }
    }
}
