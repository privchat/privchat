// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! HTTP 服务器 - 文件服务 + 管理 API 分离部署

use axum::Router;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::auth::DeviceManagerDb;
use crate::auth::{ServiceKeyManager, TokenIssueService};
use crate::http::routes;
use crate::infra::{ConnectionManager, SubscribeManager};
use crate::repository::{LoginLogRepository, PgMessageRepository, UserRepository};
use crate::security::SecurityService;
use crate::service::{AdminService, ChannelService, FileService, UploadTokenService};

/// 文件服务器共享状态
#[derive(Clone)]
pub struct FileServerState {
    pub file_service: Arc<FileService>,
    pub upload_token_service: Arc<UploadTokenService>,
}

/// 管理 API 服务器共享状态
#[derive(Clone)]
pub struct AdminServerState {
    pub service_key_manager: Arc<ServiceKeyManager>,
    pub token_issue_service: Arc<TokenIssueService>,
    pub user_repository: Arc<UserRepository>,
    pub login_log_repository: Arc<LoginLogRepository>,
    pub device_manager_db: Arc<DeviceManagerDb>,
    pub message_repository: Arc<PgMessageRepository>,
    pub channel_service: Arc<ChannelService>,
    pub connection_manager: Arc<ConnectionManager>,
    pub security_service: Arc<SecurityService>,
    pub admin_service: Arc<AdminService>,
    pub subscribe_manager: Arc<SubscribeManager>,
}

/// HTTP 文件服务器（对外，0.0.0.0）
pub struct FileHttpServer {
    state: FileServerState,
    port: u16,
}

impl FileHttpServer {
    pub fn new(
        file_service: Arc<FileService>,
        upload_token_service: Arc<UploadTokenService>,
        port: u16,
    ) -> Self {
        Self {
            state: FileServerState {
                file_service,
                upload_token_service,
            },
            port,
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .merge(routes::create_file_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());

        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        info!("🌐 HTTP 文件服务器启动在 {}", addr);

        axum::serve(listener, app).await?;
        Ok(())
    }
}

/// 管理 API 服务器（仅内网，127.0.0.1）
pub struct AdminHttpServer {
    state: AdminServerState,
    port: u16,
}

impl AdminHttpServer {
    pub fn new(
        service_key_manager: Arc<ServiceKeyManager>,
        token_issue_service: Arc<TokenIssueService>,
        user_repository: Arc<UserRepository>,
        login_log_repository: Arc<LoginLogRepository>,
        device_manager_db: Arc<DeviceManagerDb>,
        message_repository: Arc<PgMessageRepository>,
        channel_service: Arc<ChannelService>,
        connection_manager: Arc<ConnectionManager>,
        security_service: Arc<SecurityService>,
        subscribe_manager: Arc<SubscribeManager>,
        port: u16,
    ) -> Self {
        let admin_service = Arc::new(AdminService::new(
            user_repository.clone(),
            device_manager_db.clone(),
            connection_manager.clone(),
            channel_service.clone(),
            message_repository.clone(),
        ));

        Self {
            state: AdminServerState {
                service_key_manager,
                token_issue_service,
                user_repository,
                login_log_repository,
                device_manager_db,
                message_repository,
                channel_service,
                connection_manager,
                security_service,
                admin_service,
                subscribe_manager,
            },
            port,
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .merge(routes::create_admin_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());

        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;

        info!("🔒 管理 API 服务器启动在端口 {}", self.port);

        axum::serve(listener, app).await?;
        Ok(())
    }
}
