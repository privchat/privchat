//! HTTP 服务器 - 使用 Axum 提供文件服务

use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::auth::DeviceManagerDb;
use crate::auth::{ServiceKeyManager, TokenIssueService};
use crate::http::routes;
use crate::repository::{LoginLogRepository, PgMessageRepository, UserRepository};
use crate::service::{ChannelService, FileService, UploadTokenService};

/// HTTP 文件服务器共享状态
#[derive(Clone)]
pub struct HttpServerState {
    pub file_service: Arc<FileService>,
    pub upload_token_service: Arc<UploadTokenService>,
    pub token_issue_service: Arc<TokenIssueService>,
    // 管理 API 所需的服务
    pub service_key_manager: Arc<ServiceKeyManager>,
    pub user_repository: Arc<UserRepository>,
    pub login_log_repository: Arc<LoginLogRepository>,
    pub device_manager_db: Arc<DeviceManagerDb>,
    pub message_repository: Arc<PgMessageRepository>,
    pub channel_service: Arc<ChannelService>,
}

/// HTTP 文件服务器
pub struct FileHttpServer {
    state: HttpServerState,
    port: u16,
}

impl FileHttpServer {
    /// 创建新的 HTTP 文件服务器
    pub fn new(
        file_service: Arc<FileService>,
        upload_token_service: Arc<UploadTokenService>,
        token_issue_service: Arc<TokenIssueService>,
        service_key_manager: Arc<ServiceKeyManager>,
        user_repository: Arc<UserRepository>,
        login_log_repository: Arc<LoginLogRepository>,
        device_manager_db: Arc<DeviceManagerDb>,
        message_repository: Arc<PgMessageRepository>,
        channel_service: Arc<ChannelService>,
        port: u16,
    ) -> Self {
        Self {
            state: HttpServerState {
                file_service,
                upload_token_service,
                token_issue_service,
                service_key_manager,
                user_repository,
                login_log_repository,
                device_manager_db,
                message_repository,
                channel_service,
            },
            port,
        }
    }

    /// 启动 HTTP 服务器（在单独的 tokio task 中运行）
    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // 构建路由
        let app = Router::new()
            .merge(routes::create_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());

        // 绑定地址
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        info!("🌐 HTTP 文件服务器启动在端口 {}", self.port);

        // 启动服务器
        let server = axum::serve(listener, app);
        server.await?;

        Ok(())
    }
}
