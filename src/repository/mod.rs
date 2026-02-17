use crate::error::DatabaseError;
use async_trait::async_trait;

/// 基础 Repository trait
#[async_trait]
pub trait Repository {
    type Entity;
    type Key;

    /// 根据主键查找实体
    async fn find_by_id(&self, id: &Self::Key) -> Result<Option<Self::Entity>, DatabaseError>;

    /// 创建新实体
    async fn create(&self, entity: &Self::Entity) -> Result<Self::Entity, DatabaseError>;

    /// 更新实体
    async fn update(&self, entity: &Self::Entity) -> Result<Self::Entity, DatabaseError>;

    /// 删除实体
    async fn delete(&self, id: &Self::Key) -> Result<bool, DatabaseError>;

    /// 检查实体是否存在
    async fn exists(&self, id: &Self::Key) -> Result<bool, DatabaseError>;
}

/// 分页参数
#[derive(Debug, Clone)]
pub struct PaginationParams {
    pub page: u32,
    pub per_page: u32,
    pub offset: u32,
}

impl Default for PaginationParams {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: 20,
            offset: 0,
        }
    }
}

impl PaginationParams {
    pub fn new(page: u32, per_page: u32) -> Self {
        Self {
            page,
            per_page,
            offset: (page - 1) * per_page,
        }
    }
}

/// 分页结果
#[derive(Debug, Clone)]
pub struct PaginationResult<T> {
    pub data: Vec<T>,
    pub total: u64,
    pub page: u32,
    pub per_page: u32,
    pub total_pages: u32,
}

impl<T> PaginationResult<T> {
    pub fn new(data: Vec<T>, total: u64, page: u32, per_page: u32) -> Self {
        let total_pages = ((total as f64) / (per_page as f64)).ceil() as u32;
        Self {
            data,
            total,
            page,
            per_page,
            total_pages,
        }
    }
}

// 模块导出（暂时注释掉数据库相关的）
pub mod channel_repo;
pub mod device_repo;
pub mod file_upload_repo;
pub mod login_log_repository;
pub mod message_repo;
pub mod presence_repository;
pub mod user_device_repo; // ✨ 新增：用户设备 Repository
pub mod user_repo;
pub mod user_settings_repo; // ENTITY_SYNC_V1 user_settings，表为主 // 文件上传记录（持久化，有据可查）

// 重新导出 PostgreSQL Repository 实现
pub use channel_repo::{ChannelRepository, PgChannelRepository};
pub use device_repo::*;
pub use file_upload_repo::FileUploadRepository;
pub use login_log_repository::{CreateLoginLogRequest, LoginLogQuery, LoginLogRepository};
pub use message_repo::{MessageRepository, PgMessageRepository};
pub use presence_repository::PresenceRepository;
pub use user_device_repo::{UserDevice, UserDeviceRepository}; // ✨ 新增
pub use user_repo::UserRepository;
pub use user_settings_repo::UserSettingsRepository;
