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

use crate::auth::{
    DeviceManager, DeviceManagerDb, JwtService, SessionVerifyResult, TokenRevocationService,
};
use crate::context::RequestContext;
use crate::handler::MessageHandler;
use crate::model::pts::{PtsGenerator, UserMessageIndex};
use crate::model::user::DeviceInfo;
use crate::repository::MessageRepository;
use crate::service::{
    ChannelService, NotificationService, OfflineQueueService, UnreadCountService,
};
use crate::session::SessionManager;
use crate::Result;
use async_trait::async_trait;
use chrono::Utc;
use privchat_protocol::ContentMessageType;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// 连接消息处理器
pub struct ConnectMessageHandler {
    session_manager: Arc<SessionManager>,
    jwt_service: Arc<JwtService>,
    token_revocation_service: Arc<TokenRevocationService>,
    device_manager: Arc<DeviceManager>,
    device_manager_db: Arc<DeviceManagerDb>, // ✨ 新增：数据库版设备管理器
    offline_worker: Arc<crate::infra::OfflineMessageWorker>,
    message_router: Arc<crate::infra::MessageRouter>,
    // ✨ 新增：pts 和离线消息支持
    pts_generator: Arc<PtsGenerator>,
    offline_queue_service: Arc<OfflineQueueService>,
    unread_count_service: Arc<UnreadCountService>,
    // ✨ 新增：认证会话管理（用于 RPC 权限控制）
    auth_session_manager: Arc<crate::infra::SessionManager>,
    // ✨ 新增：登录日志仓库
    login_log_repository: Arc<crate::repository::LoginLogRepository>,
    // ✨ 新增：连接管理器
    connection_manager: Arc<crate::infra::ConnectionManager>,
    // ✨ 新增：通知服务（欢迎消息等推送，未来可扩展更多联系用户能力）
    notification_service: Arc<NotificationService>,
    channel_service: Arc<ChannelService>,
    message_repository: Arc<crate::repository::PgMessageRepository>,
    user_message_index: Arc<UserMessageIndex>,
    system_message_enabled: bool,
    auto_create_system_channel: bool,
    /// 认证成功后下发的欢迎消息正文
    welcome_message: String,
}

impl ConnectMessageHandler {
    pub fn new(
        session_manager: Arc<SessionManager>,
        jwt_service: Arc<JwtService>,
        token_revocation_service: Arc<TokenRevocationService>,
        device_manager: Arc<DeviceManager>,
        device_manager_db: Arc<DeviceManagerDb>, // ✨ 新增参数
        offline_worker: Arc<crate::infra::OfflineMessageWorker>,
        message_router: Arc<crate::infra::MessageRouter>,
        pts_generator: Arc<PtsGenerator>,
        offline_queue_service: Arc<OfflineQueueService>,
        unread_count_service: Arc<UnreadCountService>,
        auth_session_manager: Arc<crate::infra::SessionManager>,
        login_log_repository: Arc<crate::repository::LoginLogRepository>, // ✨ 新增参数
        connection_manager: Arc<crate::infra::ConnectionManager>,         // ✨ 新增参数
        notification_service: Arc<NotificationService>,
        channel_service: Arc<ChannelService>,
        message_repository: Arc<crate::repository::PgMessageRepository>,
        user_message_index: Arc<UserMessageIndex>,
        system_message_enabled: bool,
        auto_create_system_channel: bool,
        welcome_message: String,
    ) -> Self {
        Self {
            session_manager,
            jwt_service,
            token_revocation_service,
            device_manager,
            device_manager_db, // ✨ 新增
            offline_worker,
            message_router,
            pts_generator,
            offline_queue_service,
            unread_count_service,
            auth_session_manager,
            login_log_repository, // ✨ 新增
            connection_manager,   // ✨ 新增
            notification_service,
            channel_service,
            message_repository,
            user_message_index,
            system_message_enabled,
            auto_create_system_channel,
            welcome_message,
        }
    }
}

#[async_trait]
impl MessageHandler for ConnectMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!(
            "🔗 ConnectMessageHandler: 处理来自会话 {} 的连接请求",
            context.session_id
        );

        // 解析连接请求
        let connect_request: privchat_protocol::protocol::AuthorizationRequest =
            privchat_protocol::decode_message(&context.data).map_err(|e| {
                crate::error::ServerError::Protocol(format!("解码连接请求失败: {}", e))
            })?;

        info!(
            "🔗 ConnectMessageHandler: 设备 {} 请求连接，设备类型: {:?}",
            connect_request.device_info.device_id, connect_request.device_info.device_type
        );

        // 1. 从连接请求中提取 token（直接从 auth_token 字段获取）
        let token = &connect_request.auth_token;

        if token.is_empty() {
            warn!("❌ ConnectMessageHandler: 缺少认证 token");
            return Err(crate::error::ServerError::Unauthorized(
                "缺少认证 token".to_string(),
            ));
        }

        // 2. 验证 JWT token
        let claims = match self.jwt_service.verify_token(token) {
            Ok(claims) => {
                info!(
                    "✅ ConnectMessageHandler: Token 验证成功，用户: {}",
                    claims.sub
                );
                claims
            }
            Err(e) => {
                warn!("❌ ConnectMessageHandler: Token 验证失败: {}", e);
                return self.create_error_response("INVALID_TOKEN", "Token 验证失败");
            }
        };

        // 3. 检查 token 是否被撤销
        if self.token_revocation_service.is_revoked(&claims.jti).await {
            warn!(
                "❌ ConnectMessageHandler: Token 已被撤销 (jti: {})",
                claims.jti
            );
            return self.create_error_response("TOKEN_REVOKED", "Token 已被撤销，请重新登录");
        }

        // 4. 提取用户信息
        let user_id_str = claims.sub.clone();
        let user_id = user_id_str.parse::<u64>().map_err(|_| {
            crate::error::ServerError::BadRequest(format!(
                "Invalid user_id in token: {}",
                user_id_str
            ))
        })?;
        let device_id = claims.device_id.clone();

        // 4.1 ✨ 验证请求中的 device_id 必须与 token 中的 device_id 一致（防止设备ID被篡改）
        if connect_request.device_info.device_id != device_id {
            warn!(
                "❌ ConnectMessageHandler: 设备ID不匹配 - token_device_id={}, request_device_id={}",
                device_id, connect_request.device_info.device_id
            );
            return self.create_error_response(
                "DEVICE_ID_MISMATCH",
                "请求的设备ID与 token 绑定的设备ID不匹配，请使用注册/登录时的设备ID",
            );
        }

        // 4.5 ✨ 验证设备会话（核心验证：检查 session_state 和 session_version）
        let session_version = claims.session_version;
        match self
            .device_manager_db
            .verify_device_session(user_id, &device_id, session_version)
            .await
        {
            Ok(SessionVerifyResult::Valid { .. }) => {
                info!(
                    "✅ ConnectMessageHandler: 设备会话验证通过: user={}, device={}, version={}",
                    user_id, device_id, session_version
                );
            }
            Ok(SessionVerifyResult::DeviceNotFound) => {
                warn!(
                    "❌ ConnectMessageHandler: 设备不存在: user={}, device={}",
                    user_id, device_id
                );
                return self.create_error_response("DEVICE_NOT_FOUND", "设备不存在，请重新登录");
            }
            Ok(SessionVerifyResult::SessionInactive { state, message }) => {
                warn!(
                    "❌ ConnectMessageHandler: 设备会话状态不可用: user={}, device={}, state={:?}",
                    user_id, device_id, state
                );
                return self.create_error_response("SESSION_INACTIVE", &message);
            }
            Ok(SessionVerifyResult::VersionMismatch {
                token_version,
                current_version,
            }) => {
                warn!("❌ ConnectMessageHandler: Token 版本过期: user={}, device={}, token_v={}, current_v={}",
                      user_id, device_id, token_version, current_version);
                return self.create_error_response(
                    "TOKEN_VERSION_MISMATCH",
                    &format!(
                        "Token 已失效（版本：{} < {}），请重新登录",
                        token_version, current_version
                    ),
                );
            }
            Err(e) => {
                warn!("❌ ConnectMessageHandler: 设备会话验证失败: {}", e);
                return self.create_error_response("SESSION_VERIFY_ERROR", "设备会话验证失败");
            }
        }

        // 🔐 4.5. 绑定认证会话（用于后续 RPC 权限控制）
        // client_pts 初始化为 0，推送离线消息后更新
        self.auth_session_manager
            .bind_session(
                context.session_id.clone(),
                user_id_str.clone(),
                device_id.clone(),
                claims.clone(),
            )
            .await;

        info!(
            "🔗 ConnectMessageHandler: 用户 {} (设备 {}) 请求连接",
            user_id, device_id
        );

        // 5. 更新设备最后活跃时间（如果设备不存在，自动注册）
        if let Err(e) = self.device_manager.update_last_active(&device_id).await {
            // 设备不存在，尝试自动注册设备
            if e.to_string().contains("设备不存在") {
                debug!("设备不存在，尝试自动注册: device_id={}", device_id);
                use crate::auth::models::{Device, DeviceInfo, DeviceType};
                use chrono::Utc;

                // 从 claims 中获取设备信息并创建设备
                let device = Device {
                    device_id: device_id.clone(),
                    user_id: user_id,
                    business_system_id: claims.business_system_id.clone(),
                    device_info: DeviceInfo {
                        app_id: claims.app_id.clone(),
                        device_name: format!("设备 {}", &device_id[..8]), // 使用设备ID前8位作为默认名称
                        device_model: "Unknown".to_string(),
                        os_version: "Unknown".to_string(),
                        app_version: "1.0.0".to_string(),
                    },
                    device_type: DeviceType::from_app_id(&claims.app_id),
                    token_jti: claims.jti.clone(),
                    session_version: claims.session_version, // ✨ 新增：从 token 获取
                    session_state: crate::auth::SessionState::Active, // ✨ 新增
                    kicked_at: None,                         // ✨ 新增
                    kicked_reason: None,                     // ✨ 新增
                    last_active_at: Utc::now(),
                    created_at: Utc::now(),
                    ip_address: context.remote_addr.to_string(),
                };

                if let Err(reg_err) = self.device_manager.register_device(device).await {
                    warn!("⚠️ ConnectMessageHandler: 自动注册设备失败: {}", reg_err);
                } else {
                    info!(
                        "✅ ConnectMessageHandler: 设备已自动注册: device_id={}",
                        device_id
                    );
                }
            } else {
                warn!("⚠️ ConnectMessageHandler: 更新设备活跃时间失败: {}", e);
            }
        }

        // 6. 注册用户会话到 SessionManager
        let device_info = DeviceInfo::new(
            device_id.clone(),
            claims.app_id.clone(),
            "1.0.0".to_string(), // TODO: 从 token 或请求中获取版本
        );

        if let Err(e) = self
            .session_manager
            .add_user_session(user_id.clone(), context.session_id, device_info)
            .await
        {
            warn!("⚠️ ConnectMessageHandler: 注册用户会话失败: {}", e);
            // 不返回错误，继续处理连接请求
        } else {
            info!(
                "✅ ConnectMessageHandler: 用户 {} 会话已注册 (会话ID: {})",
                user_id, context.session_id
            );
        }

        // 6.5. 注册用户在线状态到 MessageRouter
        let session_id_str = context.session_id.to_string();
        if let Err(e) = self
            .message_router
            .register_device_online(&user_id, &device_id, &session_id_str, &claims.app_id)
            .await
        {
            warn!("⚠️ ConnectMessageHandler: 注册用户在线状态失败: {}", e);
        } else {
            info!("✅ ConnectMessageHandler: 用户 {} 在线状态已注册", user_id);
        }

        // 6.6. ✨ 注册设备连接到 ConnectionManager
        if let Err(e) = self
            .connection_manager
            .register_connection(user_id, device_id.clone(), context.session_id)
            .await
        {
            warn!("⚠️ ConnectMessageHandler: 注册设备连接失败: {}", e);
        } else {
            info!("✅ ConnectMessageHandler: 设备连接已注册到 ConnectionManager");
        }

        // ✨ Phase 3: 发布 UserOnline 事件（用于取消推送）
        if let Some(event_bus) = crate::handler::send_message_handler::get_global_event_bus() {
            let event = crate::domain::events::DomainEvent::UserOnline {
                user_id,
                device_id: device_id.clone(),
                timestamp: chrono::Utc::now().timestamp(),
            };

            if let Err(e) = event_bus.publish(event) {
                warn!("⚠️ 发布 UserOnline 事件失败: {}", e);
            } else {
                debug!(
                    "✅ UserOnline 事件已发布: user_id={}, device_id={}",
                    user_id, device_id
                );
            }
        }

        // 7. ✨ 记录登录日志（仅首次 token 认证时记录）
        // 这将记录：token 首次使用时间、设备信息、IP、风险评分等
        let first_token_auth = match self
            .record_login_log(user_id, &device_id, &claims, &connect_request, &context)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                warn!("⚠️ ConnectMessageHandler: 记录登录日志失败: {}", e);
                false
            }
        };

        // 仅在 token 首次被认证使用且 token 为“新签发”时发送登录提醒，避免重启/重连反复提示。
        let token_age_secs = Utc::now().timestamp().saturating_sub(claims.iat);
        if first_token_auth && token_age_secs <= 120 {
            // spawn 到后台，不阻塞认证响应
            let channel_service = self.channel_service.clone();
            let pts_generator = self.pts_generator.clone();
            let message_repository = self.message_repository.clone();
            let user_message_index = self.user_message_index.clone();
            let offline_queue_service = self.offline_queue_service.clone();
            let connection_manager = self.connection_manager.clone();
            let system_message_enabled = self.system_message_enabled;
            let connect_req = connect_request.clone();
            tokio::spawn(async move {
                if !system_message_enabled {
                    return;
                }
                if let Err(e) = Self::send_login_notice_message_bg(
                    user_id,
                    &connect_req,
                    channel_service,
                    pts_generator,
                    message_repository,
                    user_message_index,
                    offline_queue_service,
                    connection_manager,
                )
                .await
                {
                    warn!("⚠️ 异步发送登录通知失败: user_id={}, error={}", user_id, e);
                }
            });
        } else {
            debug!(
                "📝 跳过登录提醒: user_id={}, first_token_auth={}, token_age_secs={}",
                user_id, first_token_auth, token_age_secs
            );
        }

        // 8. READY 闸门：
        // 连接鉴权成功后仅建立会话，不立即推送离线/实时消息。
        // 客户端完成 bootstrap 后通过 sync/session_ready 显式开启补差+实时推送。

        // 9. 创建连接响应
        let connect_response = privchat_protocol::protocol::AuthorizationResponse {
            success: true,
            error_code: None,
            error_message: None,
            session_id: Some(context.session_id.to_string()),
            user_id: Some(user_id),
            connection_id: Some(format!("conn_{}", context.session_id)),
            server_info: Some(privchat_protocol::protocol::ServerInfo {
                version: "1.0.0".to_string(),
                name: "PrivChat Server".to_string(),
                features: vec!["chat".to_string(), "file_transfer".to_string()],
                max_message_size: 1024 * 1024, // 1MB
                connection_timeout: 300,       // 5分钟
            }),
            heartbeat_interval: Some(60), // 60秒心跳间隔
        };

        // 9. 编码响应
        let response_bytes = privchat_protocol::encode_message(&connect_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码连接响应失败: {}", e)))?;

        info!("✅ ConnectMessageHandler: 连接请求处理完成");

        Ok(Some(response_bytes))
    }

    fn name(&self) -> &'static str {
        "ConnectMessageHandler"
    }
}

impl ConnectMessageHandler {
    /// 创建错误响应
    fn create_error_response(
        &self,
        error_code: &str,
        error_message: &str,
    ) -> Result<Option<Vec<u8>>> {
        let connect_response = privchat_protocol::protocol::AuthorizationResponse {
            success: false,
            error_code: Some(error_code.to_string()),
            error_message: Some(error_message.to_string()),
            session_id: None,
            user_id: None,
            connection_id: None,
            server_info: None,
            heartbeat_interval: None,
        };

        let response_bytes = privchat_protocol::encode_message(&connect_response)
            .map_err(|e| crate::error::ServerError::Protocol(format!("编码错误响应失败: {}", e)))?;

        Ok(Some(response_bytes))
    }

    /// 记录登录日志
    ///
    /// 只在 token 首次使用时记录，避免日志爆炸
    async fn record_login_log(
        &self,
        user_id: u64,
        device_id: &str,
        claims: &crate::auth::ImTokenClaims,
        connect_request: &privchat_protocol::protocol::AuthorizationRequest,
        context: &RequestContext,
    ) -> anyhow::Result<bool> {
        use crate::repository::CreateLoginLogRequest;
        use uuid::Uuid;

        // 1. 检查 token 是否已被记录（防止重复记录）
        let token_jti = &claims.jti;
        if self.login_log_repository.is_token_logged(token_jti).await? {
            debug!("📝 Token {} 已记录，跳过", token_jti);
            return Ok(false);
        }

        info!(
            "📝 记录登录日志: user={}, device={}, token={}",
            user_id, device_id, token_jti
        );

        // 2. 解析设备 ID
        let device_uuid =
            Uuid::parse_str(device_id).map_err(|e| anyhow::anyhow!("Invalid device_id: {}", e))?;

        // 3. 提取设备信息
        let device_info = &connect_request.device_info;
        let device_type = format!("{:?}", device_info.device_type).to_lowercase();

        // 4. 获取 IP 地址（从上下文中）
        let ip_address = context.remote_addr.ip().to_string();

        // 5. 计算风险评分
        // TODO: 实现完整的风险评分算法
        let risk_score = self
            .calculate_risk_score(user_id, device_id, &ip_address)
            .await
            .unwrap_or(0);

        // 6. 创建登录日志
        let status: i16 = if risk_score > 70 { 1 } else { 0 }; // 0: Success, 1: Suspicious
        let log_request = CreateLoginLogRequest {
            user_id: user_id as i64,
            device_id: device_uuid,
            token_jti: token_jti.clone(),
            token_created_at: claims.iat * 1000, // 转换为毫秒
            device_type,
            device_name: Some(device_info.device_name.clone()),
            device_model: device_info.device_model.clone(),
            os_version: device_info.os_version.clone(),
            app_id: claims.app_id.clone(),
            app_version: device_info.app_version.clone(),
            ip_address,
            user_agent: None, // TODO: 从 HTTP headers 或 WebSocket headers 提取
            login_method: "connect".to_string(), // 通过 Connect 消息登录
            auth_source: Some("privchat-internal".to_string()),
            status,
            risk_score: risk_score as i16,
            is_new_device: false,   // TODO: 查询设备是否首次登录
            is_new_location: false, // TODO: 检查是否为新 IP 地址
            risk_factors: if risk_score > 50 {
                Some(vec!["high_risk_ip".to_string()]) // TODO: 实现风险因素收集
            } else {
                None
            },
            metadata: None,
        };

        // 7. 保存登录日志（spawn 到后台，不阻塞认证响应）
        let log_repo = self.login_log_repository.clone();
        let device_id_owned = device_id.to_string();
        tokio::spawn(async move {
            match log_repo.create(log_request).await {
                Ok(log) => {
                    info!(
                        "✅ 登录日志已记录: log_id={}, user={}, device={}",
                        log.log_id, user_id, device_id_owned
                    );
                    if log.is_suspicious() {
                        warn!(
                            "🚨 检测到可疑登录: user={}, device={}, risk_score={}",
                            user_id, device_id_owned, risk_score
                        );
                    }
                }
                Err(e) => {
                    warn!("⚠️ 异步记录登录日志失败: user={}, error={}", user_id, e);
                }
            }
        });

        Ok(true)
    }

    /// 计算登录风险评分 (0-100)
    ///
    /// TODO: 实现完整的风险评分算法
    async fn calculate_risk_score(
        &self,
        _user_id: u64,
        _device_id: &str,
        _ip_address: &str,
    ) -> anyhow::Result<i32> {
        // 简单实现：始终返回低风险
        // 未来可以添加：
        // - 新设备检测
        // - 新位置检测
        // - 异常登录时间检测
        // - IP 信誉检测
        // - 地理位置跳跃检测
        Ok(0)
    }

    async fn send_login_notice_message(
        &self,
        user_id: u64,
        connect_request: &privchat_protocol::protocol::AuthorizationRequest,
    ) -> anyhow::Result<()> {
        if !self.system_message_enabled {
            return Ok(());
        }

        if !self.auto_create_system_channel
            && self
                .channel_service
                .get_system_channel_id_for_user(user_id)
                .await
                .is_none()
        {
            return Ok(());
        }

        let (channel_id, _) = self
            .channel_service
            .get_or_create_direct_channel(
                user_id,
                crate::config::SYSTEM_USER_ID,
                Some("system_login_notice"),
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("get_or_create system channel failed: {}", e))?;

        let now = Utc::now();
        let message_id = crate::infra::next_message_id();
        let pts = self.pts_generator.next_pts(channel_id).await;
        let device_label = {
            let name = connect_request.device_info.device_name.trim();
            if name.is_empty() {
                format!("{:?}", connect_request.device_info.device_type).to_lowercase()
            } else {
                name.to_string()
            }
        };
        let content = format!("您的账号在 {} 设备登录了。", device_label);

        let login_notice_msg = crate::model::message::Message {
            message_id,
            channel_id,
            sender_id: crate::config::SYSTEM_USER_ID,
            pts: Some(pts as i64),
            local_message_id: Some(message_id),
            content: content.clone(),
            message_type: ContentMessageType::Text,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            reply_to_message_id: None,
            created_at: now,
            updated_at: now,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };
        self.message_repository
            .create(&login_notice_msg)
            .await
            .map_err(|e| anyhow::anyhow!("persist login notice failed: {}", e))?;

        self.user_message_index
            .add_message(user_id, pts, message_id)
            .await;

        let payload = serde_json::to_vec(&json!({ "content": content }))
            .map_err(|e| anyhow::anyhow!("encode login notice payload failed: {}", e))?;
        let push_msg = privchat_protocol::protocol::PushMessageRequest {
            setting: privchat_protocol::protocol::MessageSetting::default(),
            msg_key: format!("msg_{}", message_id),
            server_message_id: message_id,
            message_seq: 1,
            local_message_id: message_id,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: now.timestamp().max(0) as u32,
            channel_id,
            channel_type: 1,
            message_type: ContentMessageType::Text.as_u32(),
            expire: 0,
            topic: String::new(),
            from_uid: crate::config::SYSTEM_USER_ID,
            payload,
        };
        if let Err(e) = self.offline_queue_service.add(user_id, &push_msg).await {
            warn!("⚠️ 登录通知加入离线队列失败: {:?}", e);
        }
        match self
            .connection_manager
            .send_push_to_user(user_id, &push_msg)
            .await
        {
            Ok(success) => {
                debug!(
                    "✅ 登录通知实时推送完成: user_id={}, success_sessions={}",
                    user_id, success
                );
            }
            Err(e) => warn!("⚠️ 登录通知实时推送失败: {:?}", e),
        }

        let _ = self
            .channel_service
            .update_last_message(channel_id, message_id)
            .await;
        Ok(())
    }

    /// 后台发送登录通知（static 版本，用于 tokio::spawn）
    async fn send_login_notice_message_bg(
        user_id: u64,
        connect_request: &privchat_protocol::protocol::AuthorizationRequest,
        channel_service: Arc<ChannelService>,
        pts_generator: Arc<PtsGenerator>,
        message_repository: Arc<crate::repository::PgMessageRepository>,
        user_message_index: Arc<UserMessageIndex>,
        offline_queue_service: Arc<OfflineQueueService>,
        connection_manager: Arc<crate::infra::ConnectionManager>,
    ) -> anyhow::Result<()> {
        let (channel_id, _) = channel_service
            .get_or_create_direct_channel(
                user_id,
                crate::config::SYSTEM_USER_ID,
                Some("system_login_notice"),
                None,
            )
            .await
            .map_err(|e| anyhow::anyhow!("get_or_create system channel failed: {}", e))?;

        let now = Utc::now();
        let message_id = crate::infra::next_message_id();
        let pts = pts_generator.next_pts(channel_id).await;
        let device_label = {
            let name = connect_request.device_info.device_name.trim();
            if name.is_empty() {
                format!("{:?}", connect_request.device_info.device_type).to_lowercase()
            } else {
                name.to_string()
            }
        };
        let content = format!("您的账号在 {} 设备登录了。", device_label);

        let login_notice_msg = crate::model::message::Message {
            message_id,
            channel_id,
            sender_id: crate::config::SYSTEM_USER_ID,
            pts: Some(pts as i64),
            local_message_id: Some(message_id),
            content: content.clone(),
            message_type: ContentMessageType::Text,
            metadata: serde_json::Value::Object(serde_json::Map::new()),
            reply_to_message_id: None,
            created_at: now,
            updated_at: now,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };
        message_repository
            .create(&login_notice_msg)
            .await
            .map_err(|e| anyhow::anyhow!("persist login notice failed: {}", e))?;

        user_message_index
            .add_message(user_id, pts, message_id)
            .await;

        let payload = serde_json::to_vec(&json!({ "content": content }))
            .map_err(|e| anyhow::anyhow!("encode login notice payload failed: {}", e))?;
        let push_msg = privchat_protocol::protocol::PushMessageRequest {
            setting: privchat_protocol::protocol::MessageSetting::default(),
            msg_key: format!("msg_{}", message_id),
            server_message_id: message_id,
            message_seq: 1,
            local_message_id: message_id,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp: now.timestamp().max(0) as u32,
            channel_id,
            channel_type: 1,
            message_type: ContentMessageType::Text.as_u32(),
            expire: 0,
            topic: String::new(),
            from_uid: crate::config::SYSTEM_USER_ID,
            payload,
        };
        if let Err(e) = offline_queue_service.add(user_id, &push_msg).await {
            warn!("⚠️ 登录通知加入离线队列失败: {:?}", e);
        }
        match connection_manager
            .send_push_to_user(user_id, &push_msg)
            .await
        {
            Ok(success) => {
                debug!(
                    "✅ 登录通知实时推送完成: user_id={}, success_sessions={}",
                    user_id, success
                );
            }
            Err(e) => warn!("⚠️ 登录通知实时推送失败: {:?}", e),
        }

        let _ = channel_service
            .update_last_message(channel_id, message_id)
            .await;
        Ok(())
    }
}
