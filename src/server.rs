use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use msgtrans::{
    event::ServerEvent,
    protocol::{QuicServerConfig, TcpServerConfig, WebSocketServerConfig},
    transport::TransportServerBuilder,
};

use privchat_protocol::protocol::MessageType;

use crate::auth::TokenAuth;
use crate::config::ServerConfig;
use crate::dispatcher::MessageDispatcher;
use crate::error::ServerError;
use crate::handler::{
    ConnectMessageHandler, DisconnectMessageHandler, PingMessageHandler, RPCMessageHandler,
    SendMessageHandler, SubscribeMessageHandler,
};
use crate::infra::{database::Database, CacheManager, OnlineStatusManager, OnlineStatusStats};
use crate::repository::{PgChannelRepository, PgMessageRepository, UserRepository};
use crate::session::SessionManager;
// ChannelService 现在从 channel_service 导出
use crate::context::RequestContext;
use crate::service::message_history_service::MessageHistoryService;

/// 聊天服务器统计信息
#[derive(Debug, Clone)]
pub struct ServerStats {
    pub total_connections: u64,
    pub active_sessions: u64,
    pub messages_sent: u64,
    pub messages_received: u64,
    pub uptime_seconds: u64,
}

impl ServerStats {
    pub fn new() -> Self {
        Self {
            total_connections: 0,
            active_sessions: 0,
            messages_sent: 0,
            messages_received: 0,
            uptime_seconds: 0,
        }
    }
}

/// 聊天服务器
pub struct ChatServer {
    config: ServerConfig,
    token_auth: Arc<TokenAuth>,
    session_manager: Arc<SessionManager>,
    cache_manager: Arc<CacheManager>,
    online_status_manager: Arc<OnlineStatusManager>,
    stats: Arc<tokio::sync::RwLock<ServerStats>>,
    transport: Option<Arc<msgtrans::transport::TransportServer>>,
    message_dispatcher: Arc<MessageDispatcher>,
    /// 频道服务（原会话服务）
    channel_service: Arc<crate::service::ChannelService>,
    /// SendMessageHandler 的引用（用于设置 TransportServer）
    send_message_handler: Arc<SendMessageHandler>,
    /// 文件服务
    file_service: Arc<crate::service::FileService>,
    /// 上传 token 服务
    upload_token_service: Arc<crate::service::UploadTokenService>,
    /// Token 签发服务
    token_issue_service: Arc<crate::auth::TokenIssueService>,
    /// 认证会话管理器（用于 RPC 权限控制）
    auth_session_manager: Arc<crate::infra::SessionManager>,
    /// 数据库连接池
    database: Arc<Database>,
    /// 用户仓库
    user_repository: Arc<UserRepository>,
    /// 会话仓库
    channel_repository: Arc<PgChannelRepository>,
    /// 消息仓库
    message_repository: Arc<PgMessageRepository>,
    /// 安全服务（分层防护）
    security_service: Arc<crate::security::SecurityService>,
    /// 安全中间件
    security_middleware: Arc<crate::middleware::SecurityMiddleware>,
    /// 设备管理器（数据库版，用于会话管理）✨ 新增
    device_manager_db: Arc<crate::auth::DeviceManagerDb>,
    /// 登录日志仓库（用于登录记录和安全审计）✨ 新增
    login_log_repository: Arc<crate::repository::LoginLogRepository>,
    /// 连接管理器（用于管理活跃连接和设备断连）✨ 新增
    connection_manager: Arc<crate::infra::ConnectionManager>,
    /// 通知服务（向客户端推送消息等，未来可扩展更多联系用户的能力）
    notification_service: Arc<crate::service::NotificationService>,
    /// 消息路由器（维护 user/device/session 在线状态）
    message_router: Arc<crate::infra::MessageRouter>,
    /// MessageRouter 的会话发送适配器（绑定 TransportServer 后可真实下发 PushMessageRequest）
    message_router_session_adapter: Arc<crate::infra::message_router::SessionManagerAdapter>,
    /// 业务 Handler 并发限流器（STABILITY_SPEC 禁令 2）
    handler_limiter: crate::infra::handler_limiter::HandlerLimiter,
    /// 事件总线（用于 lagged 指标上报）
    event_bus: Arc<crate::infra::EventBus>,
    /// Redis 客户端（用于 pool 指标上报）
    redis_client: Option<Arc<crate::infra::redis::RedisClient>>,
    /// 离线消息 Worker（用于队列指标上报）
    offline_worker: Arc<crate::infra::OfflineMessageWorker>,
}

impl ChatServer {
    /// 创建新的聊天服务器
    pub async fn new(config: ServerConfig) -> Result<Self, ServerError> {
        info!("🔧 初始化聊天服务器组件...");

        // 🤖 初始化系统用户列表（必须在最开始）
        info!("🤖 初始化系统用户列表...");
        crate::config::init_system_users();
        info!("✅ 系统用户列表初始化完成");

        // 📊 初始化消息投递追踪（DeliveryTrace）
        info!("📊 初始化 DeliveryTrace 追踪存储...");
        crate::infra::delivery_trace::init_global_trace_store(10_000);
        info!("✅ DeliveryTrace 追踪存储初始化完成（容量=10000）");

        // 🔌 初始化数据库连接（必须在其他组件之前）
        info!("🔌 初始化数据库连接...");
        let database = Database::new(&config.database_url)
            .await
            .map_err(|e| ServerError::Internal(format!("数据库连接失败: {}", e)))?;
        let database = Arc::new(database);
        info!("✅ 数据库连接池初始化完成");

        // 📦 初始化 Repository 层
        info!("📦 初始化 Repository 层...");
        let pool = Arc::new(database.pool().clone());
        let user_repository = Arc::new(UserRepository::new(pool.clone()));
        let channel_repository = Arc::new(PgChannelRepository::new(pool.clone()));
        let message_repository = Arc::new(PgMessageRepository::new(pool.clone()));
        info!("✅ Repository 层初始化完成");

        // 🤖 系统消息功能状态
        if config.system_message.enabled {
            info!(
                "🤖 系统消息功能已启用（user_id={} 为系统用户，客户端本地化显示）",
                crate::config::SYSTEM_USER_ID
            );

            // 🔧 确保系统用户存在于数据库中（用于外键约束）
            info!("🔧 检查系统用户是否存在...");
            match user_repository
                .find_by_id(crate::config::SYSTEM_USER_ID)
                .await
            {
                Ok(Some(_)) => {
                    info!(
                        "✅ 系统用户已存在: user_id={}",
                        crate::config::SYSTEM_USER_ID
                    );
                }
                Ok(None) => {
                    info!("⚠️ 系统用户不存在，正在创建...");
                    // 创建系统用户记录
                    let system_user_def = crate::config::get_system_user(
                        crate::config::SYSTEM_USER_ID,
                    )
                    .ok_or_else(|| ServerError::Internal("系统用户定义不存在".to_string()))?;

                    let mut system_user = crate::model::user::User::new(
                        crate::config::SYSTEM_USER_ID,
                        format!("__system_{}__", crate::config::SYSTEM_USER_ID), // 使用特殊前缀确保唯一性
                    );
                    system_user.display_name = Some(system_user_def.display_name.clone());
                    system_user.user_type = 1; // 系统用户类型
                                               // 系统用户不需要密码
                    system_user.password_hash = None;

                    match user_repository.create_with_id(&system_user).await {
                        Ok(_) => {
                            info!(
                                "✅ 系统用户创建成功: user_id={}, display_name={}",
                                crate::config::SYSTEM_USER_ID,
                                system_user_def.display_name
                            );
                        }
                        Err(e) => {
                            warn!("⚠️ 系统用户创建失败: {}，可能会影响系统消息功能", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("⚠️ 检查系统用户时出错: {}，跳过创建", e);
                }
            }
        } else {
            info!("ℹ️ 系统消息功能已禁用");
        }

        let token_auth = Arc::new(TokenAuth::new());
        let session_manager = Arc::new(SessionManager::new());

        // 创建缓存管理器
        let cache_manager = Arc::new(CacheManager::new(config.cache.clone()).await?);

        // 创建在线状态管理器
        let online_status_manager =
            Arc::new(OnlineStatusManager::new(config.cache.online_status.clone()));

        // ChannelService 将在下面创建

        // 创建消息历史服务
        let message_history_service = Arc::new(MessageHistoryService::new(1000));

        let stats = Arc::new(tokio::sync::RwLock::new(ServerStats::new()));

        // 🔧 初始化认证服务
        info!("🔧 初始化认证服务...");

        // 1. 创建 JWT 服务（使用配置系统中的 jwt_secret）
        let token_ttl = 604800; // 7天
        let jwt_service = Arc::new(crate::auth::JwtService::new(
            config.jwt_secret.clone(),
            token_ttl,
        ));
        info!("✅ JWT 服务初始化完成");

        // 2. 创建 Service Key 管理器（使用主密钥模式）
        let service_master_key = std::env::var("SERVICE_MASTER_KEY").unwrap_or_else(|_| {
            "default-service-master-key-please-change-in-production".to_string()
        });
        let service_key_manager = Arc::new(crate::auth::ServiceKeyManager::new_master_key(
            service_master_key,
        ));
        info!("✅ Service Key 管理器初始化完成");

        // 3. 创建设备管理器（内存版，用于兼容性）
        let device_manager = Arc::new(crate::auth::DeviceManager::new());
        info!("✅ 设备管理器（内存版）初始化完成");

        // 3.1 创建数据库版设备管理器（✨ 新增，用于会话管理）
        let device_manager_db = Arc::new(crate::auth::DeviceManagerDb::new(pool.clone()));
        info!("✅ 设备管理器（数据库版）初始化完成");

        // 3.2 创建登录日志仓库（✨ 新增，用于登录记录和安全审计）
        let login_log_repository =
            Arc::new(crate::repository::LoginLogRepository::new(pool.clone()));
        info!("✅ 登录日志仓库初始化完成");

        // 3.3 创建连接管理器（✨ 新增，用于管理活跃连接和设备断连）
        let connection_manager = Arc::new(crate::infra::ConnectionManager::new());
        info!("✅ 连接管理器初始化完成");

        // 4. 创建 Token 撤销服务
        let token_revocation_service = Arc::new(crate::auth::TokenRevocationService::new(
            device_manager.clone(),
        ));
        info!("✅ Token 撤销服务初始化完成");

        // 🔐 5. 创建认证会话管理器（用于 RPC 权限控制）
        let auth_session_manager = Arc::new(crate::infra::SessionManager::new(24)); // 24 小时超时
        info!("✅ 认证会话管理器初始化完成");

        // 🔐 6. 创建认证中间件
        let auth_middleware = Arc::new(crate::middleware::AuthMiddleware::new(
            auth_session_manager.clone(),
        ));
        info!("✅ 认证中间件初始化完成");

        // 5. 创建 Token 签发服务
        let token_issue_service = Arc::new(crate::auth::TokenIssueService::new(
            jwt_service.clone(),
            service_key_manager.clone(), // 使用 clone，因为后面还需要用到
            device_manager.clone(),
        ));
        info!("✅ Token 签发服务初始化完成");
        info!("✅ 认证系统初始化完成");

        // 🔧 初始化消息路由和离线消息系统
        info!("🔧 初始化消息路由系统...");

        // 1. 创建简单的内存缓存用于消息路由（使用 message_router 模块的类型）
        let user_status_cache: Arc<
            dyn crate::infra::TwoLevelCache<
                crate::infra::message_router::UserId,
                crate::infra::message_router::UserOnlineStatus,
            >,
        > = Arc::new(crate::infra::L1L2Cache::local_only(
            10000,
            Duration::from_secs(3600),
        ));
        let offline_queue_cache: Arc<
            dyn crate::infra::TwoLevelCache<
                crate::infra::message_router::UserId,
                Vec<crate::infra::message_router::OfflineMessage>,
            >,
        > = Arc::new(crate::infra::L1L2Cache::local_only(
            10000,
            Duration::from_secs(3600),
        ));
        info!("✅ 消息路由缓存创建完成");

        // 2. 创建 MessageRouter (暂时使用简化的 SessionManager 适配器)
        let message_router_config = crate::infra::message_router::MessageRouterConfig::default();
        let message_router_session_adapter = Arc::new(
            crate::infra::message_router::SessionManagerAdapter::new(session_manager.clone()),
        );
        let session_manager_for_router: Arc<dyn crate::infra::message_router::SessionManager> =
            message_router_session_adapter.clone();
        let message_router = Arc::new(crate::infra::MessageRouter::new(
            message_router_config,
            user_status_cache.clone(),
            offline_queue_cache.clone(),
            session_manager_for_router,
        ));
        info!("✅ MessageRouter 创建完成");

        // 🔧 初始化 pts 同步系统（需要在 OfflineMessageWorker 之前创建）
        info!("🔧 初始化 pts 同步系统...");

        // 1. 创建 PtsGenerator
        let pts_generator = Arc::new(crate::model::pts::PtsGenerator::new());
        info!("✅ PtsGenerator 创建完成");

        // 1.5. 创建 UserMessageIndex（用于 pts -> message_id 映射）
        let user_message_index = Arc::new(crate::model::pts::UserMessageIndex::new());
        info!("✅ UserMessageIndex 创建完成");

        // 2. 创建 OfflineQueueService（使用 Redis）
        let redis_url =
            std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
        let offline_queue_service = Arc::new(crate::service::OfflineQueueService::new(&redis_url)?);
        info!("✅ OfflineQueueService 创建完成");

        // 3. 创建 OfflineMessageWorker
        let offline_worker_config = crate::infra::OfflineWorkerConfig::default();
        let offline_worker = Arc::new(crate::infra::OfflineMessageWorker::new(
            offline_worker_config,
            message_router.clone(),
            user_status_cache.clone(),
            offline_queue_cache.clone(),
            auth_session_manager.clone(),  // ✨ 用于获取 local_pts
            user_message_index.clone(),    // ✨ 用于 pts -> message_id 映射
            offline_queue_service.clone(), // ✨ 用于从 Redis 获取消息
        ));
        info!("✅ OfflineMessageWorker 创建完成");

        // 4. 启动 OfflineWorker 后台任务
        let offline_worker_clone = offline_worker.clone();
        tokio::spawn(async move {
            if let Err(e) = offline_worker_clone.start().await {
                error!("❌ OfflineWorker 启动失败: {}", e);
            }
        });
        info!("✅ OfflineWorker 后台任务已启动");
        info!("✅ 消息路由系统初始化完成");

        // 3. 创建 UnreadCountService（使用新的 cache::CacheManager）⭐
        let cache_for_unread = Arc::new(crate::infra::cache::CacheManager::new());
        let unread_count_service =
            Arc::new(crate::service::UnreadCountService::new(cache_for_unread));
        info!("✅ UnreadCountService 创建完成");
        info!("✅ pts 同步系统初始化完成");

        // 🎯 创建消息分发器，并注册处理器
        info!("🏗️ 构建消息分发器...");
        let mut message_dispatcher = MessageDispatcher::new();

        // 先注册 PingRequest；AuthorizationRequest 需在 channel_service 与 notification_service 创建后注册
        message_dispatcher.register_handler(
            MessageType::PingRequest,
            Box::new(PingMessageHandler::new()),
        );

        // 创建好友服务
        let friend_service = Arc::new(crate::service::FriendService::new(pool.clone()));

        // 创建黑名单服务
        let blacklist_service =
            Arc::new(crate::service::BlacklistService::new(cache_manager.clone()));
        info!("✅ 黑名单服务初始化完成");

        // 创建二维码服务
        let qrcode_service = Arc::new(crate::service::QRCodeService::new());
        info!("✅ 二维码服务初始化完成");

        // 创建审批服务
        let approval_service = Arc::new(crate::service::ApprovalService::new());
        info!("✅ 审批服务初始化完成");

        // 创建会话服务（需要在其他服务之前创建，因为其他服务可能依赖它）
        info!("🔧 初始化会话服务...");
        let channel_service = Arc::new(crate::service::ChannelService::new_with_repository(
            channel_repository.clone(),
        ));
        info!("✅ 会话服务初始化完成");

        // 创建通知服务（欢迎消息等推送，未来可扩展更多联系用户能力）
        let notification_service = Arc::new(crate::service::NotificationService::new());
        info!("✅ NotificationService 创建完成");

        // 注册连接/认证处理器（需 channel_service、notification_service、welcome_message）
        message_dispatcher.register_handler(
            MessageType::AuthorizationRequest,
            Box::new(ConnectMessageHandler::new(
                session_manager.clone(),
                jwt_service.clone(),
                token_revocation_service.clone(),
                device_manager.clone(),
                device_manager_db.clone(),
                offline_worker.clone(),
                message_router.clone(),
                pts_generator.clone(),
                offline_queue_service.clone(),
                unread_count_service.clone(),
                auth_session_manager.clone(),
                login_log_repository.clone(),
                connection_manager.clone(),
                notification_service.clone(),
                channel_service.clone(),
                message_repository.clone(),
                user_message_index.clone(),
                config.system_message.enabled,
                config.system_message.auto_create_channel,
                config.system_message.welcome_message.clone(),
            )),
        );
        info!("✅ ConnectMessageHandler（含欢迎 PushMessageRequest）已注册");

        // 创建隐私服务
        let privacy_service = Arc::new(crate::service::PrivacyService::new(
            cache_manager.clone(),
            channel_service.clone(),
            friend_service.clone(),
        ));

        // 创建已读回执服务
        let read_receipt_service = Arc::new(crate::service::ReadReceiptService::new());

        // 创建文件服务（多数据中心：按 current_region 或 default_storage_source_id 选择存储源）
        info!("🔧 初始化文件服务...");
        let file_storage_sources = config.effective_file_storage_sources();
        if file_storage_sources.is_empty() {
            return Err(ServerError::Internal(
                "至少需配置一个 [[file.storage_sources]]；或仅保留 [file] 下的 storage_root 与 base_url（兼容旧配置）".to_string(),
            ));
        }
        let file_service = Arc::new(crate::service::FileService::new(
            file_storage_sources,
            config.file_default_storage_source_id,
            pool.clone(),
        ));
        file_service
            .init()
            .await
            .map_err(|e| ServerError::Internal(format!("文件服务初始化失败: {}", e)))?;
        info!(
            "✅ 文件服务初始化完成（存储源数: {}，默认 id: {}）",
            file_service.source_count(),
            config.file_default_storage_source_id
        );

        // 创建消息去重服务
        info!("🔧 初始化消息去重服务...");
        let message_dedup_service = Arc::new(crate::service::MessageDedupService::new());
        message_dedup_service.start_cleanup_task(); // 启动定期清理任务
        info!("✅ 消息去重服务初始化完成");

        // 创建 @提及服务
        info!("🔧 初始化 @提及服务...");
        let mention_service = Arc::new(crate::service::MentionService::new());
        info!("✅ @提及服务初始化完成");

        // ✨ Phase 3.5: 提前创建用户设备 Repository（供 SendMessageHandler 使用）
        let user_device_repo = Arc::new(crate::repository::UserDeviceRepository::new(
            (*pool).clone(),
        ));
        info!("✅ UserDeviceRepository 创建完成（提前创建）");

        // 创建 SendMessageHandler（需要 SessionManager、FileService、ChannelService 和 MessageRouter）
        let send_message_handler = Arc::new(SendMessageHandler::new(
            message_history_service.clone(),
            session_manager.clone(),
            file_service.clone(),
            channel_service.clone(),
            message_router.clone(),
            blacklist_service.clone(),
            pts_generator.clone(),
            user_message_index.clone(),
            offline_queue_service.clone(),
            unread_count_service.clone(),
            message_dedup_service.clone(),
            privacy_service.clone(),
            friend_service.clone(),
            mention_service.clone(),
            message_repository.clone(),
            auth_session_manager.clone(),
            Some(user_device_repo.clone()), // ✨ Phase 3.5: 传递 user_device_repo
        ));

        // 5. 将 EventBus 传递给 SendMessageHandler
        // 注意：SendMessageHandler 需要支持设置 event_bus
        // 由于 SendMessageHandler 是 Arc，我们需要使用内部可变性或重新设计
        // MVP 阶段：暂时跳过，在消息发送时直接使用全局 event_bus
        // TODO: 重构 SendMessageHandler 以支持设置 event_bus

        // 创建包装器以支持 Arc
        struct SendMessageHandlerWrapper(Arc<SendMessageHandler>);
        #[async_trait::async_trait]
        impl crate::handler::MessageHandler for SendMessageHandlerWrapper {
            async fn handle(&self, context: RequestContext) -> crate::Result<Option<Vec<u8>>> {
                self.0.handle(context).await
            }
            fn name(&self) -> &'static str {
                self.0.name()
            }
        }
        message_dispatcher.register_handler(
            MessageType::SendMessageRequest,
            Box::new(SendMessageHandlerWrapper(send_message_handler.clone())),
        );

        message_dispatcher.register_handler(
            MessageType::SubscribeRequest,
            Box::new(SubscribeMessageHandler::new()),
        );
        message_dispatcher.register_handler(
            MessageType::DisconnectRequest,
            Box::new(DisconnectMessageHandler::new(connection_manager.clone())),
        ); // ✨ 新增参数
        message_dispatcher.register_handler(
            MessageType::RpcRequest,
            Box::new(RPCMessageHandler::new(auth_middleware.clone())),
        );

        // 创建上传 token 服务
        let upload_token_service = Arc::new(crate::service::UploadTokenService::new());
        info!("✅ 上传 token 服务初始化完成");

        // 创建表情包服务
        info!("🔧 初始化表情包服务...");
        let sticker_service = Arc::new(crate::service::StickerService::new());
        info!("✅ 表情包服务初始化完成");

        // 创建 Reaction 服务
        info!("🔧 初始化 Reaction 服务...");
        let reaction_service = Arc::new(crate::service::ReactionService::new());
        info!("✅ Reaction 服务初始化完成");

        // 创建在线状态管理器
        info!("🔧 初始化在线状态管理器...");
        let presence_repository =
            Arc::new(crate::repository::PresenceRepository::new((*pool).clone()));
        let presence_manager = crate::infra::PresenceManager::new(
            crate::infra::PresenceConfig::default(),
            Some(presence_repository),
        ); // 注意：PresenceManager::new() 返回 Arc<Self>
        info!("✅ 在线状态管理器初始化完成");

        // 初始化 SyncService（pts 同步机制）
        info!("🔧 初始化 SyncService...");
        let redis_config = config.cache.redis.clone().unwrap_or_else(|| {
            let url =
                std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
            crate::config::RedisConfig {
                url,
                pool_size: 50,
                min_idle: 10,
                connection_timeout_secs: 5,
                command_timeout_ms: 5000,
                idle_timeout_secs: 300,
            }
        });

        // 创建 RedisClient
        let redis_client = Arc::new(
            crate::infra::redis::RedisClient::new(&redis_config)
                .await
                .map_err(|e| ServerError::Internal(format!("Redis 客户端初始化失败: {}", e)))?,
        );
        info!("✅ RedisClient 创建完成");

        // ✨ 初始化 Push 系统（Phase 2：带 Redis 和设备查询）
        info!("🔧 初始化 Push 系统...");

        // 1. 创建 EventBus 并设置全局引用
        let event_bus = Arc::new(crate::infra::EventBus::new());
        crate::handler::send_message_handler::set_global_event_bus(event_bus.clone());
        info!("✅ EventBus 创建完成");

        // 2. 用户设备 Repository 已在上面创建（提前创建供 SendMessageHandler 使用）

        // 3. 创建 Push Planner 和 Worker 之间的通道
        let (push_tx, push_rx) = tokio::sync::mpsc::channel(1000);

        // 4. 创建共享的 Intent 状态管理器（Phase 3）
        let intent_state = Arc::new(crate::push::IntentStateManager::new());

        // 5. 创建 Push Planner（带 Redis 和状态管理器）
        let push_planner = Arc::new(crate::push::PushPlanner::with_state_manager(
            Some(redis_client.clone()),
            Arc::clone(&intent_state),
        ));
        let planner_event_bus = Arc::clone(&event_bus);
        let planner_tx = push_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = push_planner.start(planner_event_bus, planner_tx).await {
                error!("❌ PushPlanner 启动失败: {}", e);
            }
        });
        info!("✅ PushPlanner 后台任务已启动（带 Redis 在线检查 + 撤销/取消支持）");

        // 6. 创建 Push Worker（带设备 Repository 和状态管理器）
        let mut push_worker = crate::push::PushWorker::with_providers(
            push_rx,
            user_device_repo.clone(), // Clone 一份给 Worker
            intent_state,
            None, // FCM Provider（可选，需要配置）
            None, // APNs Provider（可选，需要配置）
        );
        tokio::spawn(async move {
            if let Err(e) = push_worker.start().await {
                error!("❌ PushWorker 启动失败: {}", e);
            }
        });
        info!("✅ PushWorker 后台任务已启动（带设备查询 + 撤销/取消检查）");
        info!("✅ Push 系统初始化完成（Phase 3）");

        // 创建 SyncCache（使用 redis_client 的 clone）
        let sync_cache = Arc::new(crate::service::sync::SyncCache::new(redis_client.clone()));
        info!("✅ SyncCache 创建完成");

        // 创建 DAO
        let commit_dao = Arc::new(crate::service::sync::CommitLogDao::new(database.clone()));
        let pts_dao = Arc::new(crate::service::sync::ChannelPtsDao::new(database.clone()));
        let registry_dao = Arc::new(crate::service::sync::ClientMsgRegistryDao::new(
            database.clone(),
        ));
        info!("✅ SyncService DAO 创建完成");

        // 创建 SyncService
        let sync_service = Arc::new(crate::service::sync::SyncService::new(
            pts_generator.clone(),
            commit_dao,
            pts_dao,
            registry_dao,
            sync_cache,
            channel_service.clone(),
            unread_count_service.clone(),
        ));
        info!("✅ SyncService 创建完成");

        // 初始化 RPC 系统
        info!("🔧 初始化 RPC 系统...");
        let rpc_services = crate::rpc::RpcServiceContext::new(
            // channel_service 已合并到 channel_service，不再单独传递
            message_history_service.clone(),
            cache_manager.clone(),
            presence_manager,
            friend_service.clone(),
            privacy_service.clone(),
            read_receipt_service.clone(),
            upload_token_service.clone(),
            file_service.clone(),
            sticker_service.clone(),
            channel_service.clone(),
            device_manager.clone(),
            device_manager_db.clone(), // ✨ 新增
            token_revocation_service.clone(),
            Arc::new(config.clone()),
            message_router.clone(),
            blacklist_service.clone(),
            qrcode_service.clone(),
            approval_service.clone(),
            reaction_service.clone(),
            pts_generator.clone(),
            offline_queue_service.clone(),
            user_message_index.clone(),
            jwt_service.clone(),
            user_repository.clone(),
            message_repository.clone(),
            connection_manager.clone(), // ✨ 新增
            sync_service.clone(),       // ✨ 新增
            auth_session_manager.clone(),
            offline_worker.clone(),
            user_device_repo.clone(), // ✨ Phase 3.5
            Arc::new(crate::repository::UserSettingsRepository::new(
                (*pool).clone(),
            )), // user_settings 表为主
            unread_count_service.clone(),
        );
        crate::rpc::init_rpc_system(rpc_services).await;
        info!("✅ RPC 系统初始化完成");

        // 🔐 初始化安全系统
        info!("🔐 初始化安全系统...");
        let security_config: crate::security::SecurityConfig = config.security.clone().into();
        info!("   - 安全模式: {:?}", security_config.mode);
        info!(
            "   - Shadow Ban: {}",
            if security_config.enable_shadow_ban {
                "启用"
            } else {
                "禁用"
            }
        );
        info!(
            "   - IP 封禁: {}",
            if security_config.enable_ip_ban {
                "启用"
            } else {
                "禁用"
            }
        );

        let security_service = Arc::new(crate::security::SecurityService::new(security_config));
        let security_middleware = Arc::new(crate::middleware::SecurityMiddleware::new(
            security_service.clone(),
        ));
        info!("✅ 安全系统初始化完成");

        // 初始化业务 Handler 限流器
        let handler_limiter =
            crate::infra::handler_limiter::HandlerLimiter::new(config.handler_max_inflight);
        info!(
            "✅ Handler 限流器初始化完成 (max_inflight={})",
            config.handler_max_inflight
        );

        info!("✅ 聊天服务器组件初始化完成");
        info!("📋 已注册 6 个消息处理器");

        Ok(Self {
            config,
            token_auth,
            session_manager,
            cache_manager,
            online_status_manager,
            stats,
            transport: None,
            message_dispatcher: Arc::new(message_dispatcher),
            channel_service,
            send_message_handler,
            file_service,
            upload_token_service,
            token_issue_service,
            auth_session_manager,
            database,
            user_repository,
            channel_repository,
            message_repository,
            security_service,
            security_middleware,
            device_manager_db,    // ✨ 新增
            login_log_repository, // ✨ 新增
            connection_manager,   // ✨ 新增
            notification_service,
            message_router,
            message_router_session_adapter,
            handler_limiter,
            event_bus,
            redis_client: Some(redis_client.clone()),
            offline_worker: offline_worker.clone(),
        })
    }

    /// 运行服务器主循环
    pub async fn run(&self) -> Result<(), ServerError> {
        info!("🚀 启动聊天服务器主循环...");

        // 显示配置信息
        self.show_config_info();

        // 启动 HTTP 文件服务器（在单独的 tokio task 中）
        self.start_http_server().await?;

        // 创建传输层服务器
        let transport = self.create_transport_server().await?;

        // 设置 TransportServer 到 SendMessageHandler
        self.send_message_handler
            .set_transport(transport.clone())
            .await;
        info!("✅ TransportServer 已设置到 SendMessageHandler");

        // 设置 TransportServer 到 NotificationService（欢迎等 PushMessageRequest 发送）
        self.notification_service
            .set_transport(transport.clone())
            .await;
        info!("✅ TransportServer 已设置到 NotificationService");

        self.message_router_session_adapter
            .set_transport(transport.clone())
            .await;
        info!("✅ TransportServer 已设置到 MessageRouter SessionAdapter");

        // 设置 TransportServer 到 ConnectionManager（✨ 新增）
        self.connection_manager
            .set_transport_server(transport.clone())
            .await;
        info!("✅ TransportServer 已设置到 ConnectionManager");

        // 启动事件处理
        self.start_event_handling(transport.clone()).await;

        // 启动后台任务
        self.start_background_tasks().await;

        // 启动传输层监听器
        info!("🔗 启动传输层监听器...");
        transport
            .serve()
            .await
            .map_err(|e| ServerError::Internal(format!("传输层启动失败: {}", e)))?;

        Ok(())
    }

    /// 显示配置信息
    fn show_config_info(&self) {
        info!("📊 服务器配置信息:");
        info!("  - TCP 监听地址: {}", self.config.tcp_bind_address);
        info!(
            "  - WebSocket 监听地址: {}",
            self.config.websocket_bind_address
        );
        info!("  - QUIC 监听地址: {}", self.config.quic_bind_address);
        info!("  - 最大连接数: {}", self.config.max_connections);
        info!("  - Handler 最大并发: {}", self.config.handler_max_inflight);
        info!("  - 心跳间隔: {}秒", self.config.heartbeat_interval);
        info!("  - L1 缓存内存: {}MB", self.config.cache.l1_max_memory_mb);
        info!("  - L1 缓存 TTL: {}秒", self.config.cache.l1_ttl_secs);
        info!("  - Redis L2 缓存: {}", self.config.cache.has_redis());
        info!(
            "  - 在线状态清理间隔: {}秒",
            self.config.cache.online_status.cleanup_interval_secs
        );
        info!("  - 启用协议: {:?}", self.config.enabled_protocols);
        info!("🔐 安全配置:");
        info!("  - 安全模式: {}", self.config.security.mode);
        info!(
            "  - Shadow Ban: {}",
            if self.config.security.enable_shadow_ban {
                "启用"
            } else {
                "禁用"
            }
        );
        info!(
            "  - IP 封禁: {}",
            if self.config.security.enable_ip_ban {
                "启用"
            } else {
                "禁用"
            }
        );
        info!(
            "  - 用户限流: {} tokens/s (突发: {})",
            self.config.security.rate_limit.user_tokens_per_second,
            self.config.security.rate_limit.user_burst_capacity
        );
        info!(
            "  - 会话消息限流: {} 条/s",
            self.config.security.rate_limit.channel_messages_per_second
        );
    }

    /// 创建传输层服务器
    async fn create_transport_server(
        &self,
    ) -> Result<Arc<msgtrans::transport::TransportServer>, ServerError> {
        info!("🔧 创建传输层服务器...");

        // 创建协议配置
        let tcp_config = TcpServerConfig::new(&self.config.tcp_bind_address.to_string())
            .map_err(|e| ServerError::Internal(format!("TCP配置失败: {}", e)))?;

        let websocket_config =
            WebSocketServerConfig::new(&self.config.websocket_bind_address.to_string())
                .map_err(|e| ServerError::Internal(format!("WebSocket配置失败: {}", e)))?;

        let quic_config = QuicServerConfig::new(&self.config.quic_bind_address.to_string())
            .map_err(|e| ServerError::Internal(format!("QUIC配置失败: {}", e)))?;

        // 构建传输服务器
        let transport = TransportServerBuilder::new()
            .max_connections(self.config.max_connections as usize)
            .with_protocol(tcp_config)
            .with_protocol(websocket_config)
            .with_protocol(quic_config)
            .build()
            .await
            .map_err(|e| ServerError::Internal(format!("传输服务器创建失败: {}", e)))?;

        info!("✅ 传输层服务器创建成功");
        info!("🔗 TCP 监听地址: {}", self.config.tcp_bind_address);
        info!(
            "🔗 WebSocket 监听地址: {}",
            self.config.websocket_bind_address
        );
        info!("🔗 QUIC 监听地址: {}", self.config.quic_bind_address);

        Ok(Arc::new(transport))
    }

    /// 启动事件处理
    async fn start_event_handling(&self, transport: Arc<msgtrans::transport::TransportServer>) {
        info!("🎯 启动事件处理器...");

        let mut events = transport.subscribe_events();
        let _transport_clone = transport.clone();
        let stats = self.stats.clone();
        let message_dispatcher = self.message_dispatcher.clone();
        let security_middleware = self.security_middleware.clone();
        let auth_session_manager = self.auth_session_manager.clone();
        let connection_manager = self.connection_manager.clone();
        let message_router = self.message_router.clone();
        let handler_limiter = self.handler_limiter.clone();

        tokio::spawn(async move {
            info!("📡 事件处理器开始监听...");

            while let Ok(event) = events.recv().await {
                match event {
                    ServerEvent::ConnectionEstablished { session_id, info } => {
                        info!("🔗 新连接建立: {} ({})", session_id, info.peer_addr);

                        // 🔐 安全检查：IP 连接层防护
                        let peer_ip = info.peer_addr.ip().to_string();
                        if let Err(e) = security_middleware.check_connection(&peer_ip).await {
                            warn!("🚫 连接被安全系统拒绝: {} - {:?}", peer_ip, e);
                            // 注意：这里只记录，不主动断开（由传输层处理）
                            // 如果需要主动断开，可以调用 transport.disconnect(session_id)
                            continue;
                        }

                        // 更新统计信息
                        {
                            let mut stats = stats.write().await;
                            stats.total_connections += 1;
                            stats.active_sessions += 1;
                        }
                        // 欢迎消息改为认证成功后以 PushMessageRequest 发送（见 ConnectMessageHandler），保证客户端落库
                    }
                    ServerEvent::ConnectionClosed { session_id, reason } => {
                        info!("🔌 连接关闭: {} (原因: {:?})", session_id, reason);

                        // 更新统计信息
                        {
                            let mut stats = stats.write().await;
                            stats.active_sessions = stats.active_sessions.saturating_sub(1);
                        }

                        // 清理认证会话
                        auth_session_manager.unbind_session(&session_id).await;

                        // 清理连接管理 + 消息路由在线状态
                        match connection_manager.unregister_connection(session_id).await {
                            Ok(Some((user_id, device_id))) => {
                                if let Err(e) = message_router
                                    .register_device_offline(
                                        &user_id,
                                        &device_id,
                                        Some(&session_id.to_string()),
                                    )
                                    .await
                                {
                                    warn!(
                                        "⚠️ 连接关闭后清理 MessageRouter 在线状态失败: user_id={}, device_id={}, error={}",
                                        user_id, device_id, e
                                    );
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                warn!("⚠️ 连接关闭后清理 ConnectionManager 失败: {}", e);
                            }
                        }
                    }
                    ServerEvent::MessageReceived {
                        session_id,
                        context,
                    } => {
                        // [FIX] Extract data before moving context into the async task
                        let msg_text = context.as_text_lossy();
                        let biz_type = context.biz_type;
                        let user_id = auth_session_manager.get_user_id(&session_id).await;
                        let msg_type = MessageType::from(biz_type);
                        if matches!(msg_type, MessageType::PingRequest) {
                            if let Some(user_id) = user_id {
                                debug!(
                                    "📨 收到消息并分发: {}(uid: {}) -> biz_type: {} -> MessageType: {:?} -> \"{}\"",
                                    session_id, user_id, biz_type, msg_type, msg_text
                                );
                            } else {
                                debug!(
                                    "📨 收到消息并分发: {} -> biz_type: {} -> MessageType: {:?} -> \"{}\"",
                                    session_id, biz_type, msg_type, msg_text
                                );
                            }
                        } else if let Some(user_id) = user_id {
                            info!(
                                "📨 收到消息并分发: {}(uid: {}) -> biz_type: {} -> MessageType: {:?} -> \"{}\"",
                                session_id, user_id, biz_type, msg_type, msg_text
                            );
                        } else {
                            info!(
                                "📨 收到消息并分发: {} -> biz_type: {} -> MessageType: {:?} -> \"{}\"",
                                session_id, biz_type, msg_type, msg_text
                            );
                        }

                        // 更新统计信息
                        {
                            let mut stats = stats.write().await;
                            stats.messages_received += 1;
                        }

                        // 🎯 使用消息分发器处理消息（受 HandlerLimiter 限流保护）
                        let message_dispatcher = message_dispatcher.clone();
                        let session_id_clone = session_id.clone();

                        // try_acquire: 非阻塞获取 permit，不阻塞连接层 read loop
                        match handler_limiter.try_acquire() {
                            Ok(permit) => {
                                tokio::spawn(async move {
                                    // permit 绑定在 task 内部，task 结束自动释放
                                    let _permit = permit;

                                    let msg_type = MessageType::from(biz_type);
                                    let request_context = crate::context::RequestContext::new(
                                        session_id_clone.clone(),
                                        msg_text.as_bytes().to_vec(),
                                        "127.0.0.1:0".parse().unwrap(),
                                    );

                                    match message_dispatcher
                                        .dispatch(msg_type, request_context)
                                        .await
                                    {
                                        Ok(Some(response)) => {
                                            context.respond(response);
                                            debug!("✅ 消息分发器响应已发送: {} (biz_type: {}, MessageType: {:?})", session_id_clone, biz_type, msg_type);
                                        }
                                        Ok(None) => {
                                            debug!(
                                                "消息分发器无响应: {} (biz_type: {}, MessageType: {:?})",
                                                session_id_clone, biz_type, msg_type
                                            );
                                        }
                                        Err(e) => {
                                            error!("❌ 消息分发器处理失败: {:?} - {}", msg_type, e);
                                        }
                                    }
                                });
                            }
                            Err(_) => {
                                // 限流触发：handler 并发已满
                                // Best-Effort: 丢弃 + 计数（Ping/Subscribe 等）
                                // Must-Deliver (SendMessage): 客户端未收到 ACK 会重发
                                // 不需要服务端慢路径 — 重发由客户端驱动
                                warn!(
                                    "🚫 Handler 限流触发，拒绝消息: session={}, biz_type={}",
                                    session_id, biz_type
                                );
                            }
                        }
                    }
                    ServerEvent::MessageSent {
                        session_id,
                        message_id,
                    } => {
                        debug!("📤 消息已发送: {} -> {}", session_id, message_id);

                        // 更新统计信息
                        {
                            let mut stats = stats.write().await;
                            stats.messages_sent += 1;
                        }
                    }
                    ServerEvent::TransportError { session_id, error } => {
                        warn!("⚠️ 传输错误: {:?} (会话: {:?})", error, session_id);
                    }
                    ServerEvent::ServerStarted { address } => {
                        info!("🚀 服务器启动: {}", address);
                    }
                    ServerEvent::ServerStopped => {
                        info!("🛑 服务器停止");
                    }
                }
            }

            warn!("📡 事件处理器已停止");
        });
    }

    /// 启动后台任务
    async fn start_background_tasks(&self) {
        info!("🔄 启动后台任务...");

        // 启动统计更新任务
        self.start_stats_updater().await;

        // 启动在线状态清理任务
        self.start_online_status_cleaner().await;

        // 启动缓存统计任务
        self.start_cache_stats_reporter().await;

        // 🔐 启动安全系统清理任务
        self.start_security_cleaner().await;

        info!("✅ 后台任务启动完成");
    }

    /// 启动统计更新任务
    async fn start_stats_updater(&self) {
        let stats = self.stats.clone();
        let auth_session_manager = self.auth_session_manager.clone();
        let handler_limiter = self.handler_limiter.clone();
        let event_bus = self.event_bus.clone();
        let redis_client = self.redis_client.clone();
        let database = self.database.clone();
        let offline_worker = self.offline_worker.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;

                let mut stats_guard = stats.write().await;
                stats_guard.active_sessions = auth_session_manager.session_count().await as u64;
                stats_guard.uptime_seconds += 60;

                // Handler 限流指标上报
                let inflight = handler_limiter.inflight();
                let rejected = handler_limiter.rejected_total();
                crate::infra::metrics::record_handler_inflight(inflight);
                crate::infra::metrics::record_handler_rejected(rejected);

                // EventBus lagged 指标上报
                let lagged = event_bus.lagged_total();
                crate::infra::metrics::record_event_bus_lagged(lagged);

                // Redis 连接池指标上报
                if let Some(ref redis) = redis_client {
                    let state = redis.pool_state();
                    let active = state.connections - state.idle_connections;
                    crate::infra::metrics::record_redis_pool(active, state.idle_connections);
                }

                // 数据库连接池指标上报
                let db_pool = database.pool();
                let db_size = db_pool.size();
                let db_idle = db_pool.num_idle() as u32;
                let db_active = db_size - db_idle;
                crate::infra::metrics::record_db_pool(db_active, db_idle);

                // 离线队列指标上报
                let queue_depth = offline_worker.queue_depth();
                let try_send_fail = offline_worker.try_send_fail_total();
                let fallback = offline_worker.fallback_total();
                crate::infra::metrics::record_offline_queue_depth(queue_depth);
                crate::infra::metrics::record_offline_try_send_fail(try_send_fail);
                crate::infra::metrics::record_offline_fallback(fallback);

                let secs = stats_guard.uptime_seconds;
                let days = secs / 86400;
                let hours = (secs % 86400) / 3600;
                let minutes = (secs % 3600) / 60;
                let seconds = secs % 60;
                let uptime_str = if days > 0 {
                    format!("{}天{}小时{}分{}秒", days, hours, minutes, seconds)
                } else if hours > 0 {
                    format!("{}小时{}分{}秒", hours, minutes, seconds)
                } else if minutes > 0 {
                    format!("{}分{}秒", minutes, seconds)
                } else {
                    format!("{}秒", seconds)
                };

                info!(
                    "📊 服务器统计: 在线会话={}, 总连接={}, handler={}/{}, rejected={}, lagged={}, redis_active={}, db_active={}, offline_q={}, 运行={}",
                    stats_guard.active_sessions, stats_guard.total_connections,
                    inflight, handler_limiter.max_inflight(), rejected,
                    lagged,
                    redis_client.as_ref().map(|r| r.pool_state().connections - r.pool_state().idle_connections).unwrap_or(0),
                    db_active,
                    queue_depth,
                    uptime_str
                );
            }
        });
    }

    /// 启动在线状态清理任务
    async fn start_online_status_cleaner(&self) {
        let online_status_manager = self.online_status_manager.clone();
        let cleanup_interval = self.config.cache.online_status.cleanup_interval_secs;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(cleanup_interval));
            loop {
                interval.tick().await;

                let cleaned = online_status_manager.cleanup_expired_sessions();
                if cleaned > 0 {
                    info!("🧹 清理过期会话: {} 个", cleaned);
                }
            }
        });
    }

    /// 启动缓存统计报告任务
    async fn start_cache_stats_reporter(&self) {
        let cache_manager = self.cache_manager.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(300)); // 5分钟
            loop {
                interval.tick().await;

                let stats = cache_manager.get_stats().await;
                info!(
                    "💾 缓存统计: L1命中率={:.2}%, L2命中率={:.2}%, 总请求={}",
                    stats.l1_hit_rate * 100.0,
                    stats.l2_hit_rate * 100.0,
                    stats.total_requests
                );
            }
        });

        // 🔐 9. 启动认证会话清理任务
        let auth_session_manager_clone = self.auth_session_manager.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600)); // 每小时
            loop {
                interval.tick().await;

                let cleaned = auth_session_manager_clone.cleanup_expired_sessions().await;
                info!("🔐 认证会话清理完成: 清理了 {} 个过期会话", cleaned);
            }
        });
        info!("✅ 认证会话清理任务已启动");
    }

    /// 启动安全系统清理任务
    async fn start_security_cleaner(&self) {
        let security_service = self.security_service.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(3600)); // 每小时
            loop {
                interval.tick().await;

                security_service.cleanup_expired_data().await;
                info!("🔐 安全系统清理完成");
            }
        });
        info!("✅ 安全系统清理任务已启动");
    }

    /// 模拟服务器活动
    async fn simulate_server_activity(&self) -> Result<(), ServerError> {
        info!("🎭 开始模拟服务器活动...");

        // 模拟用户上线
        self.simulate_users_online().await;

        // 模拟心跳更新
        self.simulate_heartbeat_updates().await;

        // 模拟用户活动
        self.simulate_user_activity().await;

        // 模拟部分用户下线
        self.simulate_users_offline().await;

        // 保持服务器运行
        self.keep_server_running().await;

        Ok(())
    }

    /// 模拟用户上线
    async fn simulate_users_online(&self) {
        info!("👥 模拟用户上线...");

        for i in 1..=100 {
            let session_id = format!("session_{}", i);
            let user_id = format!("user_{}", (i - 1) % 50 + 1); // 50个用户
            let device_id = format!("device_{}", i);
            let device_type = match i % 4 {
                0 => privchat_protocol::DeviceType::iOS,
                1 => privchat_protocol::DeviceType::MacOS,
                2 => privchat_protocol::DeviceType::Web,
                _ => privchat_protocol::DeviceType::Android,
            };
            let ip_address = format!("192.168.1.{}", i % 255 + 1);

            self.online_status_manager.simple_user_online(
                session_id,
                user_id,
                device_id,
                device_type,
                ip_address,
            );

            // 更新统计
            let mut stats = self.stats.write().await;
            stats.total_connections += 1;
        }

        info!("✅ 100个用户上线完成");
    }

    /// 模拟心跳更新
    async fn simulate_heartbeat_updates(&self) {
        info!("💓 模拟心跳更新...");

        for i in 1..=100 {
            let session_id = format!("session_{}", i);
            self.online_status_manager
                .simple_update_heartbeat(&session_id);
        }

        info!("✅ 心跳更新完成");
    }

    /// 模拟用户活动
    async fn simulate_user_activity(&self) {
        info!("🎯 模拟用户活动...");

        for round in 1..=3 {
            info!("  - 活动轮次 {}", round);

            // 模拟缓存操作
            for i in 1..=20 {
                let user_id = i as u64;
                let cache_key = format!("user_profile:{}", user_id);

                // 创建用户资料数据
                let user_profile = crate::infra::CachedUserProfile {
                    user_id: user_id.to_string(),
                    username: format!("user_{}", i), // 账号
                    nickname: format!("User {}", i), // 昵称
                    avatar_url: None,
                    user_type: 0, // 普通用户
                    phone: None,
                    email: None,
                };

                // 模拟缓存写入
                if let Err(e) = self
                    .cache_manager
                    .set_user_profile(user_id, user_profile)
                    .await
                {
                    warn!("缓存写入失败: {}", e);
                }

                // 模拟缓存读取
                match self.cache_manager.get_user_profile(user_id).await {
                    Ok(Some(_)) => debug!("缓存命中: {}", cache_key),
                    Ok(None) => debug!("缓存未命中: {}", cache_key),
                    Err(e) => warn!("缓存读取失败: {}", e),
                }
            }

            // 等待一段时间
            sleep(Duration::from_secs(2)).await;
        }

        info!("✅ 用户活动模拟完成");
    }

    /// 模拟部分用户下线
    async fn simulate_users_offline(&self) {
        info!("📤 模拟部分用户下线...");

        for i in 1..=20 {
            let session_id = format!("session_{}", i);
            self.online_status_manager.simple_user_offline(&session_id);
        }

        info!("✅ 20个用户下线完成");
    }

    /// 保持服务器运行
    async fn keep_server_running(&self) {
        info!("🔄 服务器进入运行状态...");
        info!("💡 提示: 按 Ctrl+C 停止服务器");

        // 主循环
        loop {
            sleep(Duration::from_secs(30)).await;

            // 显示当前状态
            let stats = self.stats.read().await;
            let online_stats = self.online_status_manager.get_stats();
            let cache_stats = self.cache_manager.get_stats().await;

            info!("🔍 服务器状态检查:");
            info!("  - 在线用户: {}", online_stats.total_users);
            info!("  - 活跃会话: {}", online_stats.total_sessions);
            info!("  - 总连接数: {}", stats.total_connections);
            info!("  - 运行时间: {}秒", stats.uptime_seconds);
            info!(
                "  - 缓存命中率: L1={:.1}%, L2={:.1}%",
                cache_stats.l1_hit_rate * 100.0,
                cache_stats.l2_hit_rate * 100.0
            );
        }
    }

    /// 获取服务器统计信息
    pub async fn get_stats(&self) -> ServerStats {
        let stats = self.stats.read().await;
        stats.clone()
    }

    /// 获取在线状态统计
    pub fn get_online_stats(&self) -> OnlineStatusStats {
        self.online_status_manager.get_stats()
    }

    /// 停止服务器
    pub async fn stop(&mut self) -> Result<(), ServerError> {
        info!("🛑 停止聊天服务器...");
        info!("✅ 聊天服务器已停止");
        Ok(())
    }

    /// 启动 HTTP 文件服务器（在单独的 tokio task 中运行）
    async fn start_http_server(&self) -> Result<(), ServerError> {
        // 初始化 Prometheus 指标（供 GET /metrics 暴露）
        if crate::infra::metrics::init().is_err() {
            // 已初始化或重复调用，忽略
        } else {
            info!("📊 Prometheus 指标已启用，GET /metrics 可用");
        }

        // 创建 Service Key 管理器（用于管理 API 认证）
        let service_master_key = std::env::var("SERVICE_MASTER_KEY").unwrap_or_else(|_| {
            "default-service-master-key-please-change-in-production".to_string()
        });
        let service_key_manager = Arc::new(crate::auth::ServiceKeyManager::new_master_key(
            service_master_key,
        ));

        // 获取 ChannelService 和 ChannelService（需要从 server 中获取）
        // 注意：这里需要访问 server 中的这些服务，但目前它们不在 Server 结构体中
        // 需要从 RPC 服务上下文中获取，或者添加到 Server 结构体中
        // 暂时使用临时方案：从全局服务中获取

        // TODO: 需要将 channel_service 和 channel_service 添加到 Server 结构体中
        // 或者从 RPC 服务上下文中获取

        // 创建 HTTP 服务器
        let http_server = crate::http::FileHttpServer::new(
            self.file_service.clone(),
            self.upload_token_service.clone(),
            self.token_issue_service.clone(),
            service_key_manager,
            self.user_repository.clone(),
            self.login_log_repository.clone(),
            self.device_manager_db.clone(),
            self.message_repository.clone(),
            self.channel_service.clone(),
            self.config.http_file_server_port,
        );

        // 在单独的 tokio task 中启动 HTTP 服务器
        tokio::spawn(async move {
            if let Err(e) = http_server.start().await {
                error!("❌ HTTP 文件服务器启动失败: {}", e);
            }
        });

        info!(
            "✅ HTTP 文件服务器已在后台启动（端口 {}）",
            self.config.http_file_server_port
        );
        Ok(())
    }
}
