// 认证模块 - 提供JWT签发、验证和设备管理功能

pub mod device_manager;
pub mod device_manager_db;
pub mod jwt_service;
pub mod models;
pub mod password;
pub mod service_key_manager;
pub mod session_state;
pub mod token;
pub mod token_issue_service;
pub mod token_revocation;

// 重新导出主要类型
pub use device_manager::{DeviceManager, DeviceStats};
pub use device_manager_db::DeviceManagerDb;
pub use jwt_service::JwtService;
pub use models::{
    Device, DeviceInfo, DeviceItem, DeviceListResponse, DeviceType, ImTokenClaims,
    IssueTokenRequest, IssueTokenResponse, ServiceKeyConfig,
};
pub use password::{hash_password, verify_password, PASSWORD_COST};
pub use service_key_manager::{ServiceKeyManager, ServiceKeyStrategy};
pub use session_state::{KickReason, KickedDevice, SessionState, SessionVerifyResult};
pub use token::TokenAuth;
pub use token_issue_service::TokenIssueService;
pub use token_revocation::TokenRevocationService;
