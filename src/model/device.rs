//! 设备模型

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use sqlx::Type;

// 重新导出 infra 模块中的 DeviceSession 类型
pub use crate::infra::DeviceSession;

/// 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type, Default)]
#[sqlx(type_name = "varchar")]
#[allow(non_camel_case_types)]
pub enum DeviceType {
    /// iOS设备 (iPhone/iPad)
    iOS,
    /// Android设备 (手机/平板)
    Android,
    /// Web浏览器
    #[default]
    Web,
    /// macOS桌面应用
    MacOS,
    /// Windows桌面应用
    Windows,
    /// Linux/Unix桌面应用 (包括 FreeBSD 等)
    Linux,
    /// IoT设备 (智能音箱、智能硬件、手表等)
    IoT,
    /// 未知设备
    Unknown,
}

impl DeviceType {
    /// 从字符串转换
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ios" => DeviceType::iOS,
            "android" => DeviceType::Android,
            "web" => DeviceType::Web,
            "macos" | "desktop" => DeviceType::MacOS,
            "windows" => DeviceType::Windows,
            "linux" | "freebsd" | "unix" => DeviceType::Linux,
            "iot" => DeviceType::IoT,
            _ => DeviceType::Unknown,
        }
    }

    /// 转换为字符串
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceType::iOS => "ios",
            DeviceType::Android => "android",
            DeviceType::Web => "web",
            DeviceType::MacOS => "macos",
            DeviceType::Windows => "windows",
            DeviceType::Linux => "linux",
            DeviceType::IoT => "iot",
            DeviceType::Unknown => "unknown",
        }
    }
}

/// 设备信息（对应 privchat_devices 表）
/// 注意：不使用 FromRow，因为有 sqlx(skip) 字段，使用 from_db_row 方法
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    /// 设备ID
    pub id: String,  // 数据库中是 Uuid，查询时转换为 String
    /// 用户ID
    pub user_id: u64,  // 数据库中是 BIGINT
    /// 设备类型
    pub device_type: DeviceType,
    /// 设备名称
    pub device_name: Option<String>,
    /// 设备型号
    pub device_model: Option<String>,
    /// 操作系统版本
    pub os_version: Option<String>,
    /// 应用版本
    pub app_version: Option<String>,
    /// 最后活跃时间（数据库存储为 BIGINT 毫秒时间戳）
    pub last_active_at: Option<DateTime<Utc>>,
    /// 创建时间（数据库存储为 BIGINT 毫秒时间戳）
    pub created_at: DateTime<Utc>,
}

impl Device {
    /// 创建新设备
    pub fn new(
        id: String,
        user_id: u64,
        device_type: DeviceType,
        device_name: Option<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            id,
            user_id,
            device_type,
            device_name,
            device_model: None,
            os_version: None,
            app_version: None,
            last_active_at: Some(now),
            created_at: now,
        }
    }

    /// 从数据库行创建（处理时间戳和类型转换）
    pub fn from_db_row(
        device_id: Uuid,
        user_id: i64,  // PostgreSQL BIGINT
        device_type: String,
        device_name: Option<String>,
        device_model: Option<String>,
        os_version: Option<String>,
        app_version: Option<String>,
        last_active_at: Option<i64>,  // 毫秒时间戳
        created_at: i64,  // 毫秒时间戳
    ) -> Self {
        Self {
            id: device_id.to_string(),
            user_id: user_id as u64,
            device_type: DeviceType::from_str(&device_type),
            device_name,
            device_model,
            os_version,
            app_version,
            last_active_at: last_active_at.and_then(|ts| DateTime::from_timestamp_millis(ts)),
            created_at: DateTime::from_timestamp_millis(created_at)
                .unwrap_or_else(|| Utc::now()),
        }
    }

    /// 转换为数据库插入值
    pub fn to_db_values(&self) -> (Uuid, i64, String, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>, i64) {
        (
            Uuid::parse_str(&self.id).unwrap_or_else(|_| Uuid::new_v4()),
            self.user_id as i64,
            self.device_type.as_str().to_string(),
            self.device_name.clone(),
            self.device_model.clone(),
            self.os_version.clone(),
            self.app_version.clone(),
            self.last_active_at.map(|dt| dt.timestamp_millis()),
            self.created_at.timestamp_millis(),
        )
    }

    /// 更新最后活跃时间
    pub fn update_last_active(&mut self) {
        self.last_active_at = Some(Utc::now());
    }
}

/// 设备信息（业务层使用的简化版本，兼容现有代码）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// 设备ID
    pub device_id: String,
    /// 设备类型
    pub device_type: String,
    /// 应用版本
    pub app_version: String,
    /// 连接时间
    pub connected_at: DateTime<Utc>,
    /// 最后活动时间
    pub last_activity: DateTime<Utc>,
}

impl DeviceInfo {
    /// 创建新设备信息
    pub fn new(device_id: String, device_type: String, app_version: String) -> Self {
        let now = Utc::now();
        Self {
            device_id,
            device_type,
            app_version,
            connected_at: now,
            last_activity: now,
        }
    }
    
    /// 更新活动时间
    pub fn update_activity(&mut self) {
        self.last_activity = Utc::now();
    }
}
