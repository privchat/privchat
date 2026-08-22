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
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::auth::DeviceManagerDb;
use crate::auth::{ServiceKeyManager, TokenIssueService};
use crate::http::routes;
use crate::infra::{ConnectionManager, SubscribeManager};
use crate::repository::{LoginLogRepository, PgMessageRepository, UserRepository};
// UserRepository is not exposed in AdminServerState — admin handlers must go through UserService.
// It is still needed as a constructor dependency for AdminService (until that also converges).
use crate::security::SecurityService;
use crate::service::{
    AdminService, ChannelService, FileService, FriendService, MessageService, RoomHistoryService,
    UploadTokenService, UserService,
};

/// 文件服务器共享状态
#[derive(Clone)]
pub struct FileServerState {
    pub file_service: Arc<FileService>,
    pub upload_token_service: Arc<UploadTokenService>,
    /// 校验登录态（`Authorization: Bearer`）。
    ///
    /// 🔴 **上传要双凭证**：上传 token 说的是「允许上传这一份文件」，它不说明
    /// 「现在是谁在操作」。token 有效期 24 小时，一旦泄露，只凭它就能读写整个上传
    /// 会话——所以每个请求还要证明自己是**签这张 token 时的那个用户**。
    ///
    /// `None` = 没接验证器（单元测试装配），此时要求登录态的端点一律拒绝，
    /// 而不是放行——缺省必须是拒绝。
    pub auth: Option<Arc<dyn UploadAuthenticator>>,
    /// S3 直传的分片后端（RESUMABLE §8.7）。`None` = 未接入（直传门禁，
    /// 实现顺序第 5 步）；接入前所有会话恒 proxy，`/files/part-url` 永远回 20616。
    pub numbered_part_backend:
        Option<Arc<dyn crate::service::numbered_parts::NumberedPartBackend>>,
    /// final key 的对象探测（RESUMABLE §8.5）：HEAD / 流式回读 / 删除三个恢复
    /// 原语，生产实现委托现有存储层（§8.7：不进 NumberedPartBackend）。与分片
    /// 后端同门禁：`None` = 未接入，S3 会话的 status/complete/abort 分支不可达。
    pub final_object_probe:
        Option<Arc<dyn crate::service::final_object_probe::FinalObjectProbe>>,
}

/// 「这个 bearer 是谁」——文件服务需要知道的**全部**。
///
/// 🔴 收成这么窄的一个接口，而不是直接依赖 `UnifiedTokenService`：文件服务只关心
/// 身份，不关心签发、刷新、吊销、设备会话版本。把整个 auth 栈拖进来，代价是它的
/// 每一个依赖（RSA 密钥、设备表、refresh 仓库）都会变成文件服务测试的前置——
/// 于是上传的测试要先把认证的 fixture 搭一遍，两件事从此绑死。
#[async_trait::async_trait]
pub trait UploadAuthenticator: Send + Sync {
    /// 有效则返回用户 id；无效返回 `Err(原因)`（原因只进日志，不回给客户端）。
    async fn user_of(&self, bearer: &str) -> std::result::Result<u64, String>;
}

#[async_trait::async_trait]
impl UploadAuthenticator for crate::auth::UnifiedTokenService {
    async fn user_of(&self, bearer: &str) -> std::result::Result<u64, String> {
        let r = self.introspect(bearer).await;
        if !r.active {
            return Err(r.reason.unwrap_or_else(|| "inactive".to_string()));
        }
        r.user_id.ok_or_else(|| "no user_id".to_string())
    }
}

/// 管理 API 服务器共享状态
#[derive(Clone)]
pub struct AdminServerState {
    pub service_key_manager: Arc<ServiceKeyManager>,
    pub token_issue_service: Arc<TokenIssueService>,
    pub login_log_repository: Arc<LoginLogRepository>,
    pub device_manager_db: Arc<DeviceManagerDb>,
    pub channel_service: Arc<ChannelService>,
    pub friend_service: Arc<FriendService>,
    pub connection_manager: Arc<ConnectionManager>,
    pub security_service: Arc<SecurityService>,
    pub admin_service: Arc<AdminService>,
    pub subscribe_manager: Arc<SubscribeManager>,
    pub room_history_service: Arc<RoomHistoryService>,
    pub message_service: Arc<MessageService>,
    pub user_service: Arc<UserService>,
    /// Web 扫码登录场景服务（spec QR_API §4）。
    pub qr_login_service: Arc<crate::service::qr_login_service::QrLoginService>,
    /// Web 扫码登录的 unauth 推送 publisher（spec QR_API §5）。
    pub qr_login_publisher: Arc<crate::service::QrLoginPublisher>,
    /// 统一 Token 编排服务：issue / refresh / introspect / revoke 端点共享一份。
    pub unified_token_service: Arc<crate::auth::UnifiedTokenService>,
    /// Room subscribe ticket 配置（spec ROOM_CHANNEL_SPEC §4）。`None` = 未配
    /// `[room_ticket]`，`/api/service/room-tickets/issue` 端点返回 503。
    pub room_ticket: Option<Arc<crate::config::RoomTicketConfig>>,
    /// 隐私服务(PROFILE_VISIBILITY P2:平台级开关 admin 读写)。
    pub privacy_service: Arc<crate::service::PrivacyService>,
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
        auth: Option<Arc<dyn UploadAuthenticator>>,
        port: u16,
    ) -> Self {
        // 直传门禁接线（第十六轮评审 P0）：默认存储源显式 `direct_upload` 时，
        // FileService::init 已在启动期构建生产后端与探测；这里拿同一份接线，
        // 不再恒 None。未开启时两者为 None，各端点回「门禁未接入」错误。
        let wiring = file_service.s3_direct();
        Self {
            state: FileServerState {
                numbered_part_backend: wiring.as_ref().map(|w| w.backend.clone()),
                final_object_probe: wiring.as_ref().map(|w| w.probe.clone()),
                file_service,
                upload_token_service,
                auth,
            },
            port,
        }
    }

    /// 对外只读暴露装配结果（第十六轮评审：真实启动链路可测）。
    pub fn state(&self) -> &FileServerState {
        &self.state
    }

    /// P1-11：bind 与 serve 分离。bind 在启动路径同步执行——端口占用/权限问题
    /// 直接阻断启动（fail-fast），不再在后台 task 里被吞成一行 error log。
    pub async fn bind(
        &self,
    ) -> Result<tokio::net::TcpListener, Box<dyn std::error::Error + Send + Sync>> {
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        info!("🌐 HTTP 文件服务器已绑定 {}", addr);
        Ok(listener)
    }

    pub async fn serve(
        &self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .merge(routes::create_file_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());
        axum::serve(listener, app).await?;
        Ok(())
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = self.bind().await?;
        self.serve(listener).await
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
        friend_service: Arc<FriendService>,
        connection_manager: Arc<ConnectionManager>,
        security_service: Arc<SecurityService>,
        subscribe_manager: Arc<SubscribeManager>,
        room_history_service: Arc<RoomHistoryService>,
        message_service: Arc<MessageService>,
        user_service: Arc<UserService>,
        qr_login_service: Arc<crate::service::qr_login_service::QrLoginService>,
        qr_login_publisher: Arc<crate::service::QrLoginPublisher>,
        unified_token_service: Arc<crate::auth::UnifiedTokenService>,
        room_ticket: Option<Arc<crate::config::RoomTicketConfig>>,
        privacy_service: Arc<crate::service::PrivacyService>,
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
                login_log_repository,
                device_manager_db,
                channel_service,
                friend_service,
                connection_manager,
                security_service,
                admin_service,
                subscribe_manager,
                room_history_service,
                message_service,
                user_service,
                qr_login_service,
                qr_login_publisher,
                unified_token_service,
                room_ticket,
                privacy_service,
            },
            port,
        }
    }

    pub async fn start(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .merge(routes::create_admin_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());

        let listener = self.bind().await?;
        self.serve_on(listener, app).await
    }

    /// P1-11：bind 与 serve 分离（同 FileHttpServer，fail-fast + supervisor 重启）。
    pub async fn bind(
        &self,
    ) -> Result<tokio::net::TcpListener, Box<dyn std::error::Error + Send + Sync>> {
        let addr = format!("0.0.0.0:{}", self.port);
        let listener = tokio::net::TcpListener::bind(&addr).await?;
        info!("🔒 管理 API 服务器已绑定端口 {}", self.port);
        Ok(listener)
    }

    pub async fn serve(
        &self,
        listener: tokio::net::TcpListener,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let app = Router::new()
            .merge(routes::create_admin_routes())
            .layer(CorsLayer::permissive())
            .with_state(self.state.clone());
        self.serve_on(listener, app).await
    }

    async fn serve_on(
        &self,
        listener: tokio::net::TcpListener,
        app: Router,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
        Ok(())
    }
}
