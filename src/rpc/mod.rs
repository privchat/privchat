pub mod types;
pub mod error;
pub mod router;
pub mod helpers;

// 系统模块
pub mod account;
pub mod contact;
pub mod device;
pub mod group;
pub mod channel_broadcast;
pub mod message;
pub mod file;
pub mod sticker;
pub mod channel;
pub mod qrcode;
pub mod user;
pub mod presence;
pub mod sync;
pub mod entity;

use std::sync::Arc;
use router::GLOBAL_RPC_ROUTER;
use types::{RPCMessageRequest, RPCMessageResponse};
use crate::service::{ChannelService, MessageHistoryService, FriendService, PrivacyService, ReadReceiptService, UploadTokenService, FileService, StickerService, BlacklistService, QRCodeService, ApprovalService, ReactionService, OfflineQueueService};
use crate::service::sync::SyncService;
use crate::auth::{DeviceManager, DeviceManagerDb, TokenRevocationService};
use crate::infra::{CacheManager, MessageRouter, PresenceManager, ConnectionManager};  // ✨ 新增 ConnectionManager
use crate::config::ServerConfig;
use crate::model::pts::{PtsGenerator, UserMessageIndex};
use crate::repository::UserRepository;

/// RPC 请求上下文 - 包含请求相关的上下文信息
#[derive(Debug, Clone)]
pub struct RpcContext {
    /// 用户ID (可选)
    pub user_id: Option<String>,
    /// 设备ID (可选)
    pub device_id: Option<String>,
    /// 请求时间戳
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl RpcContext {
    /// 创建新的 RPC 上下文
    pub fn new() -> Self {
        Self {
            user_id: None,
            device_id: None,
            timestamp: chrono::Utc::now(),
        }
    }
    
    /// 设置用户ID
    pub fn with_user_id(mut self, user_id: String) -> Self {
        self.user_id = Some(user_id);
        self
    }
    
    /// 设置设备ID
    pub fn with_device_id(mut self, device_id: String) -> Self {
        self.device_id = Some(device_id);
        self
    }
    
    /// 是否已认证
    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
}

/// RPC 服务上下文 - 包含所有业务服务的引用
#[derive(Clone)]
pub struct RpcServiceContext {
    // channel_service 已合并到 channel_service
    pub message_history_service: Arc<MessageHistoryService>,
    pub cache_manager: Arc<CacheManager>,
    pub presence_manager: Arc<PresenceManager>,
    pub friend_service: Arc<FriendService>,
    pub privacy_service: Arc<PrivacyService>,
    pub read_receipt_service: Arc<ReadReceiptService>,
    pub upload_token_service: Arc<UploadTokenService>,
    pub file_service: Arc<FileService>,
    pub sticker_service: Arc<StickerService>,
    pub channel_service: Arc<ChannelService>,
    pub device_manager: Arc<DeviceManager>,
    pub device_manager_db: Arc<DeviceManagerDb>,  // ✨ 新增：数据库版设备管理器
    pub token_revocation_service: Arc<TokenRevocationService>,
    pub config: Arc<ServerConfig>,
    pub message_router: Arc<MessageRouter>,
    pub blacklist_service: Arc<BlacklistService>,
    pub qrcode_service: Arc<QRCodeService>,
    pub approval_service: Arc<ApprovalService>,
    pub reaction_service: Arc<ReactionService>,
    pub pts_generator: Arc<PtsGenerator>,
    pub offline_queue_service: Arc<OfflineQueueService>,
    pub user_message_index: Arc<UserMessageIndex>,
    /// JWT 服务 - 用于签发和验证 JWT token
    pub jwt_service: Arc<crate::auth::JwtService>,
    /// 用户仓库 - 用于从数据库读取用户数据
    pub user_repository: Arc<UserRepository>,
    /// 消息仓库 - 用于从数据库读取消息数据
    pub message_repository: Arc<crate::repository::PgMessageRepository>,
    /// 连接管理器 - 用于管理活跃连接和设备断连
    pub connection_manager: Arc<ConnectionManager>,  // ✨ 新增
    /// 同步服务 - 用于 pts 同步机制
    pub sync_service: Arc<SyncService>,  // ✨ 新增
    /// 用户设备仓库 - 用于推送设备管理
    pub user_device_repo: Arc<crate::repository::UserDeviceRepository>,  // ✨ Phase 3.5
    /// 用户设置仓库 - ENTITY_SYNC_V1 user_settings，表为主
    pub user_settings_repo: Arc<crate::repository::UserSettingsRepository>,
}

impl RpcServiceContext {
    pub fn new(
        // channel_service 已合并到 channel_service
        message_history_service: Arc<MessageHistoryService>,
        cache_manager: Arc<CacheManager>,
        presence_manager: Arc<PresenceManager>,
        friend_service: Arc<FriendService>,
        privacy_service: Arc<PrivacyService>,
        read_receipt_service: Arc<ReadReceiptService>,
        upload_token_service: Arc<UploadTokenService>,
        file_service: Arc<FileService>,
        sticker_service: Arc<StickerService>,
        channel_service: Arc<ChannelService>,
        device_manager: Arc<DeviceManager>,
        device_manager_db: Arc<DeviceManagerDb>,  // ✨ 新增参数
        token_revocation_service: Arc<TokenRevocationService>,
        config: Arc<ServerConfig>,
        message_router: Arc<MessageRouter>,
        blacklist_service: Arc<BlacklistService>,
        qrcode_service: Arc<QRCodeService>,
        approval_service: Arc<ApprovalService>,
        reaction_service: Arc<ReactionService>,
        pts_generator: Arc<PtsGenerator>,
        offline_queue_service: Arc<OfflineQueueService>,
        user_message_index: Arc<UserMessageIndex>,
        jwt_service: Arc<crate::auth::JwtService>,
        user_repository: Arc<UserRepository>,
        message_repository: Arc<crate::repository::PgMessageRepository>,
        connection_manager: Arc<ConnectionManager>,  // ✨ 新增参数
        sync_service: Arc<SyncService>,  // ✨ 新增参数
        user_device_repo: Arc<crate::repository::UserDeviceRepository>,  // ✨ Phase 3.5
        user_settings_repo: Arc<crate::repository::UserSettingsRepository>,
    ) -> Self {
        Self {
            // channel_service 已合并到 channel_service
            message_history_service,
            cache_manager,
            presence_manager,
            friend_service,
            privacy_service,
            read_receipt_service,
            upload_token_service,
            file_service,
            sticker_service,
            channel_service,
            device_manager,
            device_manager_db,  // ✨ 新增
            token_revocation_service,
            config,
            message_router,
            blacklist_service,
            qrcode_service,
            approval_service,
            reaction_service,
            pts_generator,
            offline_queue_service,
            user_message_index,
            jwt_service,
            user_repository,
            message_repository,
            connection_manager,  // ✨ 新增
            sync_service,  // ✨ 新增
            user_device_repo,  // ✨ Phase 3.5
            user_settings_repo,
        }
    }
}

/// 初始化 RPC 系统
pub async fn init_rpc_system(services: RpcServiceContext) {
    // 注册所有路由，传入服务上下文
    account::register_routes(services.clone()).await;
    contact::register_routes(services.clone()).await;
    device::register_routes(services.clone()).await;
    group::register_routes(services.clone()).await;
    channel::register_routes(services.clone()).await;
    sync::register_routes(services.clone()).await;
    entity::register_routes(services.clone()).await;
    message::register_routes(services.clone()).await;
    file::register_routes(services.clone()).await;
    sticker::register_routes(services.clone()).await;
    qrcode::register_routes(services.clone()).await;
    user::register_routes(services.clone()).await;
    presence::register_routes(services.clone()).await;
    
    tracing::info!("🔧 RPC 系统初始化完成 (所有模块已启用: account, contact, device, group, channel, entity, message, file, sticker, qrcode, user, presence)");
}

/// 处理 RPC 请求的入口函数
pub async fn handle_rpc_request(request: RPCMessageRequest, ctx: RpcContext) -> RPCMessageResponse {
    GLOBAL_RPC_ROUTER.handle(request, ctx).await
}

/// 获取所有注册的路由列表
pub async fn list_all_routes() -> Vec<String> {
    GLOBAL_RPC_ROUTER.list_routes().await
}

// 重新导出常用类型
pub use error::{RpcError, RpcResult};
pub use router::RpcRouter;

/// 从 RpcContext 中获取已认证的 user_id (u64)
/// 
/// # 错误
/// - 如果用户未认证，返回 Unauthorized 错误
/// - 如果 user_id 格式无效，返回 ValidationError 错误
pub fn get_current_user_id(ctx: &RpcContext) -> RpcResult<u64> {
    let user_id_str = ctx.user_id
        .as_ref()
        .ok_or_else(|| RpcError::unauthorized("User not authenticated".to_string()))?;
    
    user_id_str.parse::<u64>()
        .map_err(|_| RpcError::validation("Invalid user_id format".to_string()))
}

/// 从 JSON Value 中解析 u64 ID（仅支持数字格式）
pub fn parse_u64_param(value: &serde_json::Value, field_name: &str) -> RpcResult<u64> {
    value.get(field_name)
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation(format!("{} is required (must be u64)", field_name)))
} 