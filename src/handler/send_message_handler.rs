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

use crate::context::RequestContext;
use crate::error::ServerError;
use crate::handler::MessageHandler;
use crate::infra::SessionManager as AuthSessionManager;
use crate::model::pts::PtsGenerator;
use crate::repository::{AtomicMessageCommitRequest, PgMessageRepository};
use crate::service::message_history_service::{MessageHistoryRecord, MessageHistoryService};
use crate::service::sync::get_global_sync_service;
use crate::Result;
use async_trait::async_trait;
use privchat_protocol::error_code::ErrorCode;
use privchat_protocol::{CanonicalTimelineEvent, MessagePayloadEnvelope, NewMessageEvent};
use serde_json::Value;
use std::sync::{Arc, RwLock as StdRwLock};
use tokio::sync::RwLock;
use tracing::{error, info, warn};

/// 附件加密 v1：缩略图 `thumbnail_cek` 现随消息 metadata 下发，主文件 `cek` 永不进 metadata。
/// 任何打印 message metadata/payload 的日志都必须先经此函数把 `cek`/`thumbnail_cek` 值
/// redact 成 `"***"`，CEK 绝不落日志（ATTACHMENT_ENCRYPTION_SPEC §3.2 日志红线）。
fn redact_cek_in_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                if k == "cek" || k == "thumbnail_cek" {
                    *v = Value::String("***".to_string());
                } else {
                    redact_cek_in_value(v);
                }
            }
        }
        Value::Array(arr) => arr.iter_mut().for_each(redact_cek_in_value),
        _ => {}
    }
}

/// 把消息 metadata（JSON 字符串）转成可安全打印的 redacted 形式。
fn redact_metadata_for_log(metadata: &Option<String>) -> String {
    match metadata {
        None => "None".to_string(),
        Some(s) => match serde_json::from_str::<Value>(s) {
            Ok(mut v) => {
                redact_cek_in_value(&mut v);
                v.to_string()
            }
            // 解析失败也不回退打印原文（原文可能含 cek）。
            Err(_) => "<unparseable metadata redacted>".to_string(),
        },
    }
}

/// 发送消息处理器
#[derive(Clone)]
pub struct SendMessageHandler {
    // channel_service 已合并到 channel_service
    /// 消息历史服务
    message_history_service: Arc<MessageHistoryService>,
    /// 文件服务（用于验证 file_id）
    file_service: Arc<crate::service::FileService>,
    /// 会话服务（用于更新会话列表）
    channel_service: Arc<crate::service::ChannelService>,
    /// 传输层服务器（可选，运行时设置）
    transport: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
    /// 黑名单服务（用于拦截被拉黑用户的消息）
    blacklist_service: Arc<crate::service::BlacklistService>,
    /// ✨ pts 生成器
    pts_generator: Arc<PtsGenerator>,
    /// ✨ 消息去重服务
    /// ✨ 隐私服务（用于检查非好友消息权限）
    privacy_service: Arc<crate::service::PrivacyService>,
    /// ✨ 好友服务（用于检查好友关系）
    friend_service: Arc<crate::service::FriendService>,
    /// ✨ @提及服务（用于处理@提及功能）
    mention_service: Arc<crate::service::MentionService>,
    /// ✨ 消息仓库（PostgreSQL）
    message_repository: Arc<PgMessageRepository>,
    delivery_service: Arc<crate::service::CommittedTimelineDeliveryService>,
    /// ✨ 事件总线（用于发布 Domain Events）
    event_bus: Option<Arc<crate::infra::EventBus>>,
    /// ✨ Phase 3.5: 用户设备仓库（用于推送设备查询）
    user_device_repo: Option<Arc<crate::repository::UserDeviceRepository>>,
    /// 认证会话管理器（用于 READY 推送闸门）
    auth_session_manager: Arc<AuthSessionManager>,
    /// ServerEvent 出站 client（用于 fire-and-forget 向 application 投递
    /// `system_user.message_received` 等事件；spec SERVER_EVENT_DISPATCH_SPEC §11.1）。
    /// None = 未配 `[server_event]` 段，server 内部 emit 全部跳过。
    server_event_client: Option<Arc<crate::server_event::ServerEventClient>>,
    /// 用户仓库（用于 direct channel 对端 user_type 查询，判定是否触发
    /// `system_user.message_received` emit）。
    user_repository: Option<Arc<crate::repository::UserRepository>>,
    /// 缓存管理器（user_type 查询的 L1 入口）。
    cache_manager: Option<Arc<crate::infra::CacheManager>>,
}

// 临时全局 EventBus（MVP 阶段简化方案）
// TODO: 未来应该通过依赖注入传递
static GLOBAL_EVENT_BUS: StdRwLock<Option<Arc<crate::infra::EventBus>>> = StdRwLock::new(None);

pub fn set_global_event_bus(event_bus: Arc<crate::infra::EventBus>) {
    match GLOBAL_EVENT_BUS.write() {
        Ok(mut guard) => {
            *guard = Some(event_bus);
        }
        Err(poisoned) => {
            warn!("⚠️ GLOBAL_EVENT_BUS lock poisoned during set; recovering");
            *poisoned.into_inner() = Some(event_bus);
        }
    }
}

/// 获取全局 EventBus（供其他模块使用）
pub fn get_global_event_bus() -> Option<Arc<crate::infra::EventBus>> {
    match GLOBAL_EVENT_BUS.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            warn!("⚠️ GLOBAL_EVENT_BUS lock poisoned during get; recovering");
            poisoned.into_inner().clone()
        }
    }
}

impl SendMessageHandler {
    pub fn new(
        // channel_service 已合并到 channel_service
        message_history_service: Arc<MessageHistoryService>,
        file_service: Arc<crate::service::FileService>,
        channel_service: Arc<crate::service::ChannelService>,
        blacklist_service: Arc<crate::service::BlacklistService>,
        pts_generator: Arc<PtsGenerator>,
        privacy_service: Arc<crate::service::PrivacyService>,
        friend_service: Arc<crate::service::FriendService>,
        mention_service: Arc<crate::service::MentionService>,
        message_repository: Arc<PgMessageRepository>,
        auth_session_manager: Arc<AuthSessionManager>,
        delivery_service: Arc<crate::service::CommittedTimelineDeliveryService>,
        user_device_repo: Option<Arc<crate::repository::UserDeviceRepository>>, // ✨ Phase 3.5
    ) -> Self {
        Self {
            // channel_service 已合并到 channel_service
            message_history_service,
            file_service,
            channel_service,
            transport: Arc::new(RwLock::new(None)),
            blacklist_service,
            pts_generator,
            privacy_service,
            friend_service,
            mention_service,
            message_repository,
            delivery_service,
            event_bus: None,
            auth_session_manager,
            user_device_repo, // ✨ Phase 3.5
            server_event_client: None,
            user_repository: None,
            cache_manager: None,
        }
    }

    /// Inject the deps needed for `system_user.message_received` emit
    /// (spec SERVER_EVENT_DISPATCH_SPEC §11.1)。
    ///
    /// Server boot 期间 `SendMessageHandler::new` 在 `server_event_client` 创建之前
    /// 就被实例化，因此用 setter pattern；调用方在两者都就绪后注入。
    pub fn set_system_user_event_deps(
        &mut self,
        server_event_client: Option<Arc<crate::server_event::ServerEventClient>>,
        user_repository: Arc<crate::repository::UserRepository>,
        cache_manager: Arc<crate::infra::CacheManager>,
    ) {
        self.server_event_client = server_event_client;
        self.user_repository = Some(user_repository);
        self.cache_manager = Some(cache_manager);
    }

    /// 设置事件总线（在服务器启动后调用）
    pub fn set_event_bus(&mut self, event_bus: Arc<crate::infra::EventBus>) {
        self.event_bus = Some(event_bus);
    }

    /// 设置传输层服务器（在服务器启动后调用）
    pub async fn set_transport(&self, transport: Arc<msgtrans::transport::TransportServer>) {
        *self.transport.write().await = Some(transport);
    }

    /// CODEX-8：客户端幂等键 = (uid, device, local_message_id) 三元组。device 来自认证会话
    /// （服务端权威，不信 payload）—— 同账号多设备 / 不同用户的雪花 local_message_id 碰撞
    /// 不再互相判重（此前 `client:{uid}:{lmid}` 缺 device 维度）。
    fn client_dedup_key(sender_id: u64, device_id: &str, local_message_id: u64) -> String {
        format!("client:{}:{}:{}", sender_id, device_id, local_message_id)
    }

    /// CODEX-8 复审 P0#3/P1#4：从认证会话解析权威 (sender_uid, device_id)。纯函数便于单测。
    /// `session` = 会话的 (user_id_str, device_id)（None = 无会话）。`payload_from_uid` = 客户端
    /// 声明的发送者（协议默认 0 = 未提供）。返回 Err(ErrorCode) 应直接拒绝：
    ///   - 无会话 / uid 非法 / device 为空 → [ErrorCode::AuthRequired]（fail closed）
    ///   - payload_from_uid != 0 且 != 认证 uid → [ErrorCode::PermissionDenied]（防冒充）
    fn resolve_authenticated_sender(
        session: Option<(String, String)>,
        payload_from_uid: u64,
    ) -> std::result::Result<(u64, String), ErrorCode> {
        let (uid_str, device) = session.ok_or(ErrorCode::AuthRequired)?;
        let uid = uid_str
            .parse::<u64>()
            .map_err(|_| ErrorCode::AuthRequired)?;
        if device.is_empty() {
            return Err(ErrorCode::AuthRequired);
        }
        if payload_from_uid != 0 && payload_from_uid != uid {
            return Err(ErrorCode::PermissionDenied);
        }
        Ok((uid, device))
    }

    fn message_to_history_record(message: &crate::model::message::Message) -> MessageHistoryRecord {
        MessageHistoryRecord {
            message_id: message.message_id,
            channel_id: message.channel_id,
            sender_id: message.sender_id,
            content: message.content.clone(),
            message_type: message.message_type.as_u32(),
            seq: message.pts.unwrap_or(0).max(0) as u64,
            created_at: message.created_at,
            updated_at: message.updated_at,
            is_deleted: message.deleted,
            deleted_at: message.deleted_at,
            is_revoked: message.revoked,
            revoked_at: message.revoked_at,
            revoker_id: message.revoked_by,
            reply_to_message_id: message.reply_to_message_id,
            metadata: Some(message.metadata.to_string()),
        }
    }

    fn channel_type_code(channel_type: crate::model::channel::ChannelType) -> u8 {
        match channel_type {
            crate::model::channel::ChannelType::Direct => 1,
            crate::model::channel::ChannelType::Group => 2,
            crate::model::channel::ChannelType::Room => 3,
        }
    }
}

#[async_trait]
impl MessageHandler for SendMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!(
            "📢 SendMessageHandler: 处理来自会话 {} 的消息发送请求",
            context.session_id
        );

        // 1. 解析发送请求
        let send_message_request: privchat_protocol::protocol::SendMessageRequest =
            privchat_protocol::decode_message(&context.data)
                .map_err(|e| ServerError::Protocol(format!("解码发送请求失败: {}", e)))?;

        info!(
            "📤 SendMessageHandler: 处理发送请求 - 用户: {}, 频道: {}, 内容长度: {}",
            send_message_request.from_uid,
            send_message_request.channel_id,
            send_message_request.payload.len()
        );

        // CODEX-8 复审 P0#3/P1#4：**sender 与 device 一律取自认证会话，不信 payload**。
        // 认证会话必然带 uid+device；缺失即异常会话 —— fail closed 拒绝（否则可冒充 sender、
        // 或所有异常会话落入同一空 device 幂等命名空间）。
        let session = self
            .auth_session_manager
            .get_session_info(&context.session_id)
            .await
            .map(|s| (s.user_id, s.device_id));
        let (from_uid, sender_device_id) =
            match Self::resolve_authenticated_sender(session, send_message_request.from_uid) {
                Ok(v) => v,
                Err(code) => {
                    let msg = if code == ErrorCode::PermissionDenied {
                        "from_uid 与认证用户不一致"
                    } else {
                        "会话缺少认证身份（uid/device）"
                    };
                    return self
                        .create_error_response(&send_message_request, code, msg)
                        .await;
                }
            };
        let channel_id = send_message_request.channel_id;
        // local_message_id=0 视为「客户端未提供幂等键」（协议默认值），不参与 dedup。
        // 否则所有 0 值请求共享 `client:{uid}:{device}:0`，第一条永久占坑，后续消息会被
        // 静默判重并返回第一条的 message_id —— 等价于永久丢消息。
        let client_dedup_key = (send_message_request.local_message_id != 0).then(|| {
            Self::client_dedup_key(
                from_uid,
                &sender_device_id,
                send_message_request.local_message_id,
            )
        });

        // ✨ 1.4. 防御性检查：拒绝无效的 channel_id
        if channel_id == 0 {
            error!(
                "❌ SendMessageHandler: 无效的 channel_id: 0（用户: {}）",
                from_uid
            );
            return self
                .create_error_response(
                    &send_message_request,
                    ErrorCode::InvalidParams,
                    "无效的频道ID",
                )
                .await;
        }

        // 1.5. DB 级幂等预查（server 重启后仍生效）。local_message_id=0 无幂等键，跳过。
        if let Some(existing_message) = match client_dedup_key.as_deref() {
            Some(key) => self
                .message_repository
                .find_message_by_dedup_key(key)
                .await
                .map_err(|e| ServerError::Database(format!("查询消息幂等键失败: {}", e)))?,
            None => None,
        } {
            let existing_pts = existing_message.pts.unwrap_or(0).max(0) as u64;
            let existing_record = Self::message_to_history_record(&existing_message);
            info!(
                "🔄 SendMessageHandler: DB 幂等命中 - user={}, local_message_id={}, message_id={}, pts={}",
                from_uid,
                send_message_request.local_message_id,
                existing_message.message_id,
                existing_pts
            );
            return self
                .create_success_response(&send_message_request, &existing_record, existing_pts)
                .await;
        }

        // 2. 验证频道存在性和用户权限
        let mut channel = match self.channel_service.get_channel(&channel_id).await {
            Ok(channel) => {
                // 频道已存在，检查用户是否在成员列表中
                // 如果用户不在成员列表中，尝试从数据库会话中获取参与者信息
                if !channel.members.contains_key(&from_uid) {
                    // ✨ 从数据库会话中检查用户是否是参与者
                    if let Ok(db_channel) = self.channel_service.get_channel(&channel_id).await {
                        let is_participant = match channel.channel_type {
                            crate::model::channel::ChannelType::Direct => {
                                // 私聊：检查 direct_user1_id 和 direct_user2_id
                                // ✨ 优先检查内存中的成员列表，然后检查数据库会话的参与者
                                if channel.members.contains_key(&from_uid) {
                                    true
                                } else {
                                    // 从数据库会话的 direct_user1_id 和 direct_user2_id 检查
                                    db_channel
                                        .direct_user1_id
                                        .map(|id| id == from_uid)
                                        .unwrap_or(false)
                                        || db_channel
                                            .direct_user2_id
                                            .map(|id| id == from_uid)
                                            .unwrap_or(false)
                                }
                            }
                            crate::model::channel::ChannelType::Group => {
                                // 群聊：检查成员列表
                                // ✨ 优先检查内存中的成员列表，如果为空则从数据库查询参与者
                                if !channel.members.is_empty() {
                                    channel.members.contains_key(&from_uid)
                                } else {
                                    // 从数据库查询参与者
                                    if let Ok(participants) = self
                                        .channel_service
                                        .get_channel_participants(channel_id)
                                        .await
                                    {
                                        participants.iter().any(|p| p.user_id == from_uid)
                                    } else {
                                        false
                                    }
                                }
                            }
                            _ => false,
                        };

                        if is_participant {
                            // 用户是会话参与者，添加到频道
                            let role = match db_channel.channel_type {
                                crate::model::channel::ChannelType::Direct => {
                                    info!(
                                        "🔧 SendMessageHandler: 用户 {} 不在私聊频道 {} 的成员列表中，但从数据库会话中发现是参与者，自动添加",
                                        from_uid, channel_id
                                    );
                                    crate::model::channel::MemberRole::Member
                                }
                                crate::model::channel::ChannelType::Group => {
                                    // 从数据库查询用户的角色
                                    let participant_role = if channel.members.is_empty() {
                                        // 如果内存中的成员列表为空，从数据库查询
                                        if let Ok(participants) = self
                                            .channel_service
                                            .get_channel_participants(channel_id)
                                            .await
                                        {
                                            participants
                                                .iter()
                                                .find(|p| p.user_id == from_uid)
                                                .map(|p| p.role.clone())
                                                .unwrap_or(
                                                    crate::model::channel::MemberRole::Member,
                                                )
                                        } else {
                                            crate::model::channel::MemberRole::Member
                                        }
                                    } else {
                                        channel
                                            .members
                                            .get(&from_uid)
                                            .map(|m| m.role.clone())
                                            .unwrap_or(crate::model::channel::MemberRole::Member)
                                    };

                                    info!(
                                        "🔧 SendMessageHandler: 用户 {} 不在群聊频道 {} 的成员列表中，但从数据库会话中发现是参与者（角色: {:?}），自动添加",
                                        from_uid, channel_id, participant_role
                                    );

                                    match participant_role {
                                        crate::model::channel::MemberRole::Owner => {
                                            crate::model::channel::MemberRole::Owner
                                        }
                                        crate::model::channel::MemberRole::Admin => {
                                            crate::model::channel::MemberRole::Admin
                                        }
                                        crate::model::channel::MemberRole::Member => {
                                            crate::model::channel::MemberRole::Member
                                        }
                                    }
                                }
                                _ => crate::model::channel::MemberRole::Member,
                            };

                            if let Err(e) = self
                                .channel_service
                                .join_channel(channel_id, from_uid, Some(role))
                                .await
                            {
                                warn!("❌ SendMessageHandler: 添加用户到频道失败: {}", e);
                                return self
                                    .create_error_response(
                                        &send_message_request,
                                        ErrorCode::PermissionDenied,
                                        "无法加入频道",
                                    )
                                    .await;
                            }
                            // 重新获取频道（包含新成员）
                            self.channel_service
                                .get_channel(&channel_id)
                                .await
                                .map_err(|e| {
                                    ServerError::Internal(format!("获取频道失败: {}", e))
                                })?
                        } else {
                            // 用户不是参与者，拒绝访问
                            warn!(
                                "❌ SendMessageHandler: 用户 {} 不是频道 {} 的参与者",
                                from_uid, channel_id
                            );
                            return self
                                .create_error_response(
                                    &send_message_request,
                                    ErrorCode::PermissionDenied,
                                    "无权限访问此频道",
                                )
                                .await;
                        }
                    } else {
                        // 会话不存在，拒绝访问
                        warn!("❌ SendMessageHandler: 会话 {} 不存在", channel_id);
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::ChannelNotFound,
                                "会话不存在",
                            )
                            .await;
                    }
                } else {
                    channel
                }
            }
            Err(_) => {
                // ✨ 频道不存在，尝试恢复或创建
                // 先尝试从数据库会话中恢复频道（channel_id可能是UUID格式的channel_id）
                if let Ok(channel) = self
                    .channel_service
                    .get_channel(&send_message_request.channel_id)
                    .await
                {
                    // 会话存在，尝试恢复频道
                    info!(
                        "🔧 SendMessageHandler: 频道 {} 不存在但会话存在，尝试恢复频道",
                        send_message_request.channel_id
                    );

                    // 根据会话类型创建频道
                    match channel.channel_type {
                        crate::model::channel::ChannelType::Direct => {
                            // 私聊：从 direct_user1_id 和 direct_user2_id 获取两个用户ID
                            if let (Some(user1_id), Some(user2_id)) =
                                (channel.direct_user1_id, channel.direct_user2_id)
                            {
                                // ✨ 验证发送者是否是会话的参与者
                                if from_uid != user1_id && from_uid != user2_id {
                                    warn!(
                                        "❌ SendMessageHandler: 用户 {} 不是私聊会话 {} 的参与者",
                                        from_uid, channel_id
                                    );
                                    return self
                                        .create_error_response(
                                            &send_message_request,
                                            ErrorCode::PermissionDenied,
                                            "无权限访问此频道",
                                        )
                                        .await;
                                }

                                // 使用会话ID创建私聊频道
                                if let Err(e) = self
                                    .channel_service
                                    .create_private_chat_with_id(user1_id, user2_id, channel_id)
                                    .await
                                {
                                    warn!("❌ SendMessageHandler: 恢复私聊频道失败: {}", e);
                                    return self
                                        .create_error_response(
                                            &send_message_request,
                                            ErrorCode::InternalError,
                                            "无法恢复频道",
                                        )
                                        .await;
                                }

                                info!("✅ SendMessageHandler: 私聊频道恢复成功: {}", channel_id);

                                // 重新获取频道并验证发送者在成员列表中
                                let channel = self
                                    .channel_service
                                    .get_channel(&channel_id)
                                    .await
                                    .map_err(|e| {
                                    ServerError::Internal(format!("获取频道失败: {}", e))
                                })?;

                                // ✨ 双重验证：确保发送者在成员列表中
                                if !channel.members.contains_key(&from_uid) {
                                    warn!(
                                        "❌ SendMessageHandler: 恢复私聊频道后，发送者 {} 不在成员列表中",
                                        send_message_request.from_uid
                                    );
                                    return self
                                        .create_error_response(
                                            &send_message_request,
                                            ErrorCode::PermissionDenied,
                                            "无权限访问此频道",
                                        )
                                        .await;
                                }

                                channel
                            } else {
                                warn!("❌ SendMessageHandler: 私聊会话缺少用户ID: {}", channel_id);
                                return self
                                    .create_error_response(
                                        &send_message_request,
                                        ErrorCode::ChannelNotFound,
                                        "会话缺少用户ID",
                                    )
                                    .await;
                            }
                        }
                        crate::model::channel::ChannelType::Group => {
                            // 群聊：从数据库查询参与者并恢复频道
                            info!("🔧 SendMessageHandler: 恢复群聊频道: {}", channel_id);

                            // 从数据库查询参与者
                            let participants = match self
                                .channel_service
                                .get_channel_participants(channel_id)
                                .await
                            {
                                Ok(participants) => participants,
                                Err(e) => {
                                    warn!("❌ SendMessageHandler: 查询群聊参与者失败: {}", e);
                                    return self
                                        .create_error_response(
                                            &send_message_request,
                                            ErrorCode::InternalError,
                                            "无法查询群聊参与者",
                                        )
                                        .await;
                                }
                            };

                            if participants.is_empty() {
                                warn!("❌ SendMessageHandler: 群聊 {} 没有参与者", channel_id);
                                return self
                                    .create_error_response(
                                        &send_message_request,
                                        ErrorCode::ChannelNotFound,
                                        "群聊没有参与者",
                                    )
                                    .await;
                            }

                            // 获取群组名称（从会话元数据或使用默认值）
                            let group_name = channel
                                .metadata
                                .name
                                .clone()
                                .unwrap_or_else(|| format!("群聊 {}", channel_id));

                            // 找到群主（Owner 角色）或第一个参与者作为创建者
                            let owner_id = participants
                                .iter()
                                .find(|p| p.role == crate::model::channel::MemberRole::Owner)
                                .map(|p| p.user_id)
                                .or_else(|| participants.first().map(|p| p.user_id))
                                .ok_or_else(|| {
                                    warn!("❌ SendMessageHandler: 无法确定群主");
                                    ServerError::Internal("无法确定群主".to_string())
                                })?;

                            // 创建群聊频道
                            if let Err(e) = self
                                .channel_service
                                .create_group_chat_with_id(owner_id, group_name, channel_id)
                                .await
                            {
                                warn!("❌ SendMessageHandler: 恢复群聊频道失败: {}", e);
                                return self
                                    .create_error_response(
                                        &send_message_request,
                                        ErrorCode::InternalError,
                                        "无法恢复群聊频道",
                                    )
                                    .await;
                            }

                            // 添加所有参与者到频道
                            for participant in &participants {
                                let user_id = participant.user_id;
                                let channel_role = match participant.role {
                                    crate::model::channel::MemberRole::Owner => {
                                        crate::model::channel::MemberRole::Owner
                                    }
                                    crate::model::channel::MemberRole::Admin => {
                                        crate::model::channel::MemberRole::Admin
                                    }
                                    crate::model::channel::MemberRole::Member => {
                                        crate::model::channel::MemberRole::Member
                                    }
                                };

                                // 跳过创建者（已在 create_group_chat_with_id 中添加）
                                if user_id == owner_id {
                                    continue;
                                }

                                // 添加成员到频道
                                if let Err(e) = self
                                    .channel_service
                                    .add_member_to_group(channel_id, user_id)
                                    .await
                                {
                                    warn!(
                                        "⚠️ SendMessageHandler: 添加成员 {} 到群聊频道失败: {}",
                                        user_id, e
                                    );
                                    // 继续添加其他成员，不中断流程
                                } else {
                                    // 设置成员角色
                                    if let Err(e) = self
                                        .channel_service
                                        .set_member_role(&channel_id, &user_id, channel_role)
                                        .await
                                    {
                                        warn!(
                                            "⚠️ SendMessageHandler: 设置成员 {} 角色失败: {}",
                                            user_id, e
                                        );
                                    }
                                }
                            }

                            info!(
                                "✅ SendMessageHandler: 群聊频道恢复成功: {} ({} 个成员)",
                                channel_id,
                                participants.len()
                            );

                            // 重新获取频道
                            let channel = self
                                .channel_service
                                .get_channel(&channel_id)
                                .await
                                .map_err(|e| {
                                    ServerError::Internal(format!("获取恢复的频道失败: {}", e))
                                })?;

                            // ✨ 验证发送者是否在成员列表中
                            if !channel.members.contains_key(&from_uid) {
                                // 发送者不在成员列表中，尝试从参与者列表中添加
                                if let Some(participant) = participants
                                    .iter()
                                    .find(|p| p.user_id == send_message_request.from_uid)
                                {
                                    let channel_role = match participant.role {
                                        crate::model::channel::MemberRole::Owner => {
                                            crate::model::channel::MemberRole::Owner
                                        }
                                        crate::model::channel::MemberRole::Admin => {
                                            crate::model::channel::MemberRole::Admin
                                        }
                                        crate::model::channel::MemberRole::Member => {
                                            crate::model::channel::MemberRole::Member
                                        }
                                    };

                                    info!(
                                        "🔧 SendMessageHandler: 发送者 {} 不在恢复的群聊频道成员列表中，从数据库参与者信息中添加",
                                        send_message_request.from_uid
                                    );

                                    if let Err(e) = self
                                        .channel_service
                                        .join_channel(
                                            send_message_request.channel_id,
                                            send_message_request.from_uid.clone(),
                                            Some(channel_role),
                                        )
                                        .await
                                    {
                                        warn!(
                                            "❌ SendMessageHandler: 添加发送者到恢复的群聊频道失败: {}",
                                            e
                                        );
                                        return self
                                            .create_error_response(
                                                &send_message_request,
                                                ErrorCode::PermissionDenied,
                                                "无法加入频道",
                                            )
                                            .await;
                                    }

                                    // 重新获取频道
                                    self.channel_service
                                        .get_channel(&send_message_request.channel_id)
                                        .await
                                        .map_err(|e| {
                                            ServerError::Internal(format!("获取频道失败: {}", e))
                                        })?
                                } else {
                                    warn!(
                                        "❌ SendMessageHandler: 发送者 {} 不是群聊 {} 的参与者",
                                        send_message_request.from_uid,
                                        send_message_request.channel_id
                                    );
                                    return self
                                        .create_error_response(
                                            &send_message_request,
                                            ErrorCode::PermissionDenied,
                                            "无权限访问此频道",
                                        )
                                        .await;
                                }
                            } else {
                                channel
                            }
                        }
                        crate::model::channel::ChannelType::Room => {
                            // Room 频道不支持 SendMessage，消息通过 Admin API 广播
                            warn!(
                                "❌ SendMessageHandler: Room 频道不支持 SendMessage: {}",
                                send_message_request.channel_id
                            );
                            return self
                                .create_error_response(
                                    &send_message_request,
                                    ErrorCode::OperationNotAllowed,
                                    "Room channels do not accept SendMessage",
                                )
                                .await;
                        }
                    }
                } else {
                    // ✨ 频道和会话都不存在
                    // channel_id 现在是 u64，直接返回错误
                    warn!(
                        "❌ SendMessageHandler: 频道 {} 不存在且会话也不存在",
                        send_message_request.channel_id
                    );
                    return self
                        .create_error_response(
                            &send_message_request,
                            ErrorCode::ChannelNotFound,
                            "频道不存在",
                        )
                        .await;
                }
            }
        };

        // 3. ✨ 确保发送者在频道成员列表中（如果不在，从数据库恢复）
        if !channel.members.contains_key(&from_uid) {
            // 用户不在内存频道中，尝试从数据库恢复
            if let Ok(db_channel) = self
                .channel_service
                .get_channel(&send_message_request.channel_id)
                .await
            {
                // 检查用户是否是会话参与者
                let is_participant = match db_channel.channel_type {
                    crate::model::channel::ChannelType::Group => {
                        // 群聊：从数据库查询参与者
                        if let Ok(participants) = self
                            .channel_service
                            .get_channel_participants(send_message_request.channel_id)
                            .await
                        {
                            participants
                                .iter()
                                .any(|p| p.user_id == send_message_request.from_uid)
                        } else {
                            false
                        }
                    }
                    crate::model::channel::ChannelType::Direct => {
                        // 私聊：检查 direct_user1_id 和 direct_user2_id
                        db_channel
                            .direct_user1_id
                            .map(|id| id == from_uid)
                            .unwrap_or(false)
                            || db_channel
                                .direct_user2_id
                                .map(|id| id == from_uid)
                                .unwrap_or(false)
                    }
                    _ => false,
                };

                if is_participant {
                    // 用户是参与者，添加到频道
                    let role = match db_channel.channel_type {
                        crate::model::channel::ChannelType::Group => {
                            // 从数据库查询用户的角色
                            if let Ok(participants) = self
                                .channel_service
                                .get_channel_participants(channel_id)
                                .await
                            {
                                participants
                                    .iter()
                                    .find(|p| p.user_id == from_uid)
                                    .map(|p| match p.role {
                                        crate::model::channel::MemberRole::Owner => {
                                            crate::model::channel::MemberRole::Owner
                                        }
                                        crate::model::channel::MemberRole::Admin => {
                                            crate::model::channel::MemberRole::Admin
                                        }
                                        crate::model::channel::MemberRole::Member => {
                                            crate::model::channel::MemberRole::Member
                                        }
                                    })
                                    .unwrap_or(crate::model::channel::MemberRole::Member)
                            } else {
                                crate::model::channel::MemberRole::Member
                            }
                        }
                        _ => crate::model::channel::MemberRole::Member,
                    };

                    info!(
                        "🔧 SendMessageHandler: 用户 {} 不在频道 {} 的成员列表中，但从数据库会话中发现是参与者，自动添加",
                        from_uid, channel_id
                    );

                    if let Err(e) = self
                        .channel_service
                        .join_channel(channel_id, from_uid, Some(role))
                        .await
                    {
                        warn!("❌ SendMessageHandler: 添加用户到频道失败: {}", e);
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::PermissionDenied,
                                "无法加入频道",
                            )
                            .await;
                    }

                    // 重新获取频道（包含新成员）
                    channel = self
                        .channel_service
                        .get_channel(&send_message_request.channel_id)
                        .await
                        .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))?;
                } else {
                    // 用户不是参与者，拒绝访问
                    warn!(
                        "❌ SendMessageHandler: 用户 {} 不是频道 {} 的参与者",
                        send_message_request.from_uid, send_message_request.channel_id
                    );
                    return self
                        .create_error_response(
                            &send_message_request,
                            ErrorCode::PermissionDenied,
                            "无权限访问此频道",
                        )
                        .await;
                }
            } else {
                // 会话不存在，拒绝访问
                warn!(
                    "❌ SendMessageHandler: 用户 {} 不在频道 {} 的成员列表中，且会话不存在",
                    send_message_request.from_uid, send_message_request.channel_id
                );
                return self
                    .create_error_response(
                        &send_message_request,
                        ErrorCode::PermissionDenied,
                        "无权限访问此频道",
                    )
                    .await;
            }
        }

        // 3. ✅ 群组权限检查（禁言、全员禁言、发送权限）- 在 can_user_post 之前检查，以便返回正确的错误码
        if channel.channel_type == crate::model::channel::ChannelType::Group {
            // 获取成员信息
            if let Some(member) = channel.members.get(&from_uid) {
                // 3.1.1. 检查个人禁言状态（优先检查，返回错误码5）
                if member.is_muted {
                    let now = chrono::Utc::now();
                    if crate::model::channel::mute_is_active(
                        member.is_muted,
                        member.mute_until,
                        now,
                    ) {
                        let reject =
                            crate::model::channel::mute_reject_message(member.mute_until, now);
                        warn!(
                            "❌ SendMessageHandler: 用户 {} 在群 {} 中被禁言（mute_until={:?}）",
                            send_message_request.from_uid,
                            send_message_request.channel_id,
                            member.mute_until
                        );
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::MemberMuted,
                                &reject,
                            )
                            .await;
                    }
                    // 临时禁言已到期：放行本条消息，异步懒清理 DB + 缓存（此处持成员引用，不能同步取写锁）。
                    let cs = self.channel_service.clone();
                    let (lazy_channel_id, lazy_uid) = (channel.id, from_uid);
                    tokio::spawn(async move {
                        if let Err(e) = cs
                            .set_member_muted(&lazy_channel_id, &lazy_uid, false, None)
                            .await
                        {
                            tracing::warn!(
                                "mute lazy-clear failed: channel={} uid={} err={}",
                                lazy_channel_id,
                                lazy_uid,
                                e
                            );
                        }
                    });
                }

                // 3.1.2. 检查全员禁言（群主/管理员不受影响）。
                //   全员禁言是强权限：以 DB(privchat_groups.all_muted) 为真源，server 重启后仍生效，
                //   不依赖可能丢失的内存缓存。仅对非群主/管理员成员查询，避免拖慢热路径。
                let is_privileged = matches!(
                    member.role,
                    crate::model::channel::MemberRole::Owner
                        | crate::model::channel::MemberRole::Admin
                );
                if !is_privileged {
                    let all_muted = if let Some(gid) = channel.group_id {
                        self.channel_service
                            .get_group_policy(gid)
                            .await
                            .ok()
                            .flatten()
                            .map(|p| p.all_muted)
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    if all_muted {
                        warn!(
                            "❌ SendMessageHandler: 群 {} 全员禁言中，用户 {} 无权发言",
                            send_message_request.channel_id, send_message_request.from_uid
                        );
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::GroupMuted,
                                "群组全员禁言中",
                            )
                            .await;
                    }
                }

                // 3.1.3. 检查发送消息权限（基于角色的细粒度权限）
                let channel_role = match member.role {
                    crate::model::channel::MemberRole::Owner => {
                        crate::model::channel::MemberRole::Owner
                    }
                    crate::model::channel::MemberRole::Admin => {
                        crate::model::channel::MemberRole::Admin
                    }
                    crate::model::channel::MemberRole::Member => {
                        crate::model::channel::MemberRole::Member
                    }
                };
                let permissions = crate::model::channel::MemberPermissions::from_role(channel_role);
                if !permissions.can_send_message {
                    warn!(
                        "❌ SendMessageHandler: 用户 {} 在群 {} 中没有发送消息权限",
                        send_message_request.from_uid, send_message_request.channel_id
                    );
                    return self
                        .create_error_response(
                            &send_message_request,
                            ErrorCode::PermissionDenied,
                            "您没有发送消息权限",
                        )
                        .await;
                }
            }
        }

        // 3.2. 检查用户是否可以发送消息（基础权限检查）- 在禁言检查之后
        if !channel.can_user_post(&from_uid) {
            // 如果用户不在成员列表中，错误已经在前面处理了
            // 这里主要是检查频道设置（如 allow_member_post）
            warn!(
                "❌ SendMessageHandler: 用户 {} 无权限在频道 {} 发送消息（频道设置限制）",
                send_message_request.from_uid, send_message_request.channel_id
            );
            return self
                .create_error_response(
                    &send_message_request,
                    ErrorCode::PermissionDenied,
                    "无权限发送消息",
                )
                .await;
        }

        // 4. 消息类型来自协议层 message_type（u32），payload 仅解析 content + metadata + reply_to_message_id + mentioned_user_ids + message_source
        let content_message_type =
            privchat_protocol::ContentMessageType::from_u32(send_message_request.message_type)
                .ok_or_else(|| ServerError::Validation("无效的 message_type".to_string()))?;
        let (content, metadata, reply_to_message_id, mentioned_user_ids, message_source) =
            match Self::parse_payload(&send_message_request.payload) {
                Ok(t) => t,
                Err(e) => {
                    // 二进制守卫（生产乱码事故回归）：解不了的二进制 payload 直接拒发 20006——
                    // 绝不能 lossy-stringify 落库（会存成不可见控制符+长度字节+正文的永久脏数据，
                    // 且未剥 NUL 时 Postgres UTF8 列直接报错）。
                    warn!(
                        "⚠️ SendMessageHandler: payload 既非 envelope 也非文本，拒绝发送: {}",
                        e
                    );
                    return self
                        .create_error_response(
                            &send_message_request,
                            ErrorCode::MessageContentInvalid,
                            "无法解析的消息负载",
                        )
                        .await;
                }
            };

        info!(
            "📩 SendMessageHandler: message_type={}, content={}, metadata={}, reply_to={:?}, mentioned={:?}, source={:?}",
            send_message_request.message_type,
            content,
            redact_metadata_for_log(&metadata),
            reply_to_message_id,
            mentioned_user_ids,
            message_source
        );

        // 3.5. ✅ 检查好友关系、黑名单和非好友消息权限（仅限私聊）
        if channel.channel_type == crate::model::channel::ChannelType::Direct {
            // 获取频道的所有成员（私聊应该只有2个成员）
            let members: Vec<u64> = channel.get_member_ids();

            // 找出接收者（不是发送者的那个用户）
            let receiver_id = members.iter().find(|&id| *id != from_uid).copied();

            if let Some(receiver_id) = receiver_id {
                // 3.5.1. 先检查好友关系 — 好友直接放行，跳过黑名单和隐私检查
                let are_friends = self.friend_service.is_friend(from_uid, receiver_id).await;

                if !are_friends {
                    // 非好友 → 检查黑名单和隐私设置
                    // 3.5.2. 检查双向拉黑关系
                    let (sender_blocks_receiver, receiver_blocks_sender) = self
                        .blacklist_service
                        .check_mutual_block(from_uid, receiver_id)
                        .await
                        .unwrap_or((false, false));

                    if receiver_blocks_sender {
                        warn!(
                            "🚫 SendMessageHandler: 用户 {} 已被 {} 拉黑，无法发送消息",
                            from_uid, receiver_id
                        );
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::BlockedByUser,
                                "您已被对方拉黑，无法发送消息",
                            )
                            .await;
                    }

                    if sender_blocks_receiver {
                        warn!(
                            "🚫 SendMessageHandler: 用户 {} 已拉黑 {}，无法发送消息",
                            from_uid, receiver_id
                        );
                        return self
                            .create_error_response(
                                &send_message_request,
                                ErrorCode::UserInBlacklist,
                                "您已拉黑该用户，无法发送消息",
                            )
                            .await;
                    }

                    // 3.5.3. 检查非好友消息权限
                    match self
                        .privacy_service
                        .get_or_create_privacy_settings(receiver_id)
                        .await
                    {
                        Ok(privacy_settings) => {
                            if !privacy_settings.allow_receive_message_from_non_friend {
                                warn!(
                                    "🚫 SendMessageHandler: 用户 {} 不允许接收非好友消息，发送者 {} 不是好友",
                                    receiver_id, from_uid
                                );
                                return self
                                    .create_error_response(
                                        &send_message_request,
                                        ErrorCode::PermissionDenied,
                                        "对方设置了仅接收好友消息，无法发送",
                                    )
                                    .await;
                            } else {
                                info!(
                                    "✅ SendMessageHandler: 用户 {} 允许接收非好友消息，发送者 {} 可以发送",
                                    receiver_id, from_uid
                                );

                                if let Some(ref source) = message_source {
                                    info!(
                                        "📝 SendMessageHandler: 记录非好友消息来源: {} -> {} (source: {:?})",
                                        from_uid, receiver_id, source
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "⚠️ SendMessageHandler: 获取用户 {} 隐私设置失败: {}，默认允许非好友消息",
                                receiver_id, e
                            );
                        }
                    }
                }
            }
        }

        // 4.5. reply_to_message_id 只做格式校验，不校验存在性/归属频道
        // 按 REPLY_SPEC / local-first 语义：服务端仅转发引用的 server_message_id；
        // 接收端 UI 在本地查不到原消息时自行渲染「已删除的消息」。
        let reply_to_id_str: Option<String> = if let Some(ref reply_to_id) = reply_to_message_id {
            if reply_to_id.parse::<u64>().is_err() {
                warn!(
                    "❌ SendMessageHandler: 无效的 reply_to_message_id 格式: {}",
                    reply_to_id
                );
                return self
                    .create_error_response(
                        &send_message_request,
                        ErrorCode::InvalidParams,
                        "无效的引用消息ID格式",
                    )
                    .await;
            }
            Some(reply_to_id.clone())
        } else {
            None
        };

        // 4.6. 验证消息类型和 metadata（特别是 file_id）
        if let Err(e) = self
            .validate_message_metadata(content_message_type, &metadata, from_uid)
            .await
        {
            warn!("❌ SendMessageHandler: 消息验证失败: {}", e);
            return self
                .create_error_response(
                    &send_message_request,
                    ErrorCode::MessageContentInvalid,
                    &format!("消息验证失败: {}", e),
                )
                .await;
        }

        // 5. ✨ 处理@提及（使用客户端传递的用户ID列表，类似 Telegram）
        // 客户端在输入 @ 时已经选择了用户，所以直接使用 mentioned_user_ids
        // 消息内容中仍然显示 @用户昵称，但实际关联的是用户ID
        let is_mention_all = content.contains("@全体成员")
            || content.contains("@all")
            || content.contains("@everyone");

        // ✨ 5.1.5. 持久化消息、pts、commit 和附件绑定。
        // channel_id 现在直接是 u64，使用 channel.id 作为 channel_id。
        let channel_id = channel.id;

        // 首先确保会话存在
        self.ensure_channel_and_members(channel_id, &channel).await;

        // 验证会话是否存在（通过数据库）
        // ✨ 如果会话仍然不存在，可能是异步创建延迟，等待一小段时间后重试
        let mut retry_count = 0;
        while self.channel_service.get_channel(&channel_id).await.is_err() && retry_count < 3 {
            retry_count += 1;
            warn!(
                "⚠️ SendMessageHandler: 会话 {} 不存在，重试 {} (可能是异步创建延迟)",
                channel_id, retry_count
            );
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            // 再次尝试确保会话存在
            self.ensure_channel_and_members(channel_id, &channel).await;
        }

        // 最终验证会话是否存在
        if self.channel_service.get_channel(&channel_id).await.is_err() {
            error!(
                "❌ SendMessageHandler: 会话 {} 不存在，无法保存消息",
                channel_id
            );
            return self
                .create_error_response(
                    &send_message_request,
                    ErrorCode::ChannelNotFound,
                    "会话不存在",
                )
                .await;
        }

        // 创建 Message 模型
        use crate::model::message::Message;
        let metadata_value = if let Some(ref meta_str) = metadata {
            serde_json::from_str(meta_str)
                .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };

        let message_id = crate::infra::next_message_id();

        // 解析 reply_to_message_id 为 u64（如果存在）
        let reply_to_id = reply_to_message_id.and_then(|id| id.parse::<u64>().ok());
        let now = chrono::Utc::now();

        let message = Message {
            message_id,
            channel_id,
            sender_id: from_uid,
            pts: None,
            local_message_id: Some(send_message_request.local_message_id),
            content: content.clone(),
            message_type: content_message_type,
            metadata: metadata_value.clone(),
            reply_to_message_id: reply_to_id,
            created_at: now,
            updated_at: now,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };

        // 附件 file→message 绑定（ATTACHMENT_ENCRYPTION_SPEC §授权）：把
        // file_id / thumbnail_file_id 绑定到 message_id（business_type="message"），使接收端
        // get_url 能按 channel 成员授权访问/拿 CEK。绑定、消息、commit 在同一 DB tx 内提交。
        let bind_file_ids = Self::extract_attachment_file_ids(&message.metadata);
        let channel_type_code = Self::channel_type_code(channel.channel_type);
        let mut canonical_payload = privchat_protocol::decode_message::<MessagePayloadEnvelope>(
            &send_message_request.payload,
        )
        .unwrap_or_else(|_| MessagePayloadEnvelope {
            content: content.clone(),
            metadata: privchat_protocol::MessageMetadata::from_json_value(
                content_message_type,
                &metadata_value,
            ),
            reply_to_message_id: reply_to_id,
            mentioned_user_ids: mentioned_user_ids.clone(),
            message_source: None,
        });
        canonical_payload.content = content.clone();
        let canonical_event = CanonicalTimelineEvent::NewMessage(NewMessageEvent {
            message_type: content_message_type,
            payload: canonical_payload,
        });
        let (_, commit_content) = canonical_event
            .to_legacy_commit(channel_id, channel_type_code)
            .map_err(|error| ServerError::Protocol(format!("构造兼容消息投影失败: {error}")))?;
        let tx_result = match self
            .message_repository
            .create_message_and_commit_atomic(AtomicMessageCommitRequest {
                message,
                // None（local_message_id=0）时事务内不 claim dedup key，退化为无幂等发送。
                dedup_key: client_dedup_key.clone(),
                client_registry_claim: None,
                attachment_file_ids: bind_file_ids,
                channel_type: channel_type_code as i16,
                event: canonical_event,
                sender_username: None,
            })
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "❌ SendMessageHandler: 事务化保存消息失败 channel_id={} user={} local_message_id={}: {}",
                    channel_id, from_uid, send_message_request.local_message_id, e
                );
                return self
                    .create_error_response(
                        &send_message_request,
                        ErrorCode::DatabaseError,
                        &format!("保存消息失败: {}", e),
                    )
                    .await;
            }
        };

        let crate::repository::message_repo::AtomicMessageCommitResult {
            message,
            inserted,
            event_id,
            event_schema_version,
            canonical_event,
        } = tx_result;
        let pts = message.pts.unwrap_or(0).max(0) as u64;
        let message_record = Self::message_to_history_record(&message);

        if !inserted {
            info!(
                "🔄 SendMessageHandler: 并发 DB 幂等命中 - user={}, local_message_id={}, message_id={}, pts={}",
                from_uid, send_message_request.local_message_id, message.message_id, pts
            );
            return self
                .create_success_response(&send_message_request, &message_record, pts)
                .await;
        }

        if let Err(e) = self
            .message_history_service
            .store_messages_batch(vec![message_record.clone()])
            .await
        {
            warn!(
                "⚠️ SendMessageHandler: 事务提交后写入内存历史失败 message_id={}: {}",
                message.message_id, e
            );
        }

        info!(
            "✅ SendMessageHandler: 消息/PTS/Commit 已事务化保存 message_id={}, pts={}",
            message.message_id, pts
        );

        let commit = privchat_protocol::rpc::sync::ServerCommit {
            event_id,
            pts,
            server_msg_id: message.message_id,
            local_message_id: Some(send_message_request.local_message_id),
            channel_id,
            channel_type: channel_type_code,
            message_type: content_message_type.as_str().to_string(),
            content: commit_content,
            server_timestamp: message.created_at.timestamp_millis(),
            sender_id: from_uid,
            sender_info: None,
            event_schema_version,
            canonical_event,
        };

        if let Some(sync_service) = get_global_sync_service() {
            if let Err(e) = sync_service.cache_committed_commit(&commit).await {
                warn!(
                    "⚠️ SendMessageHandler: 事务提交后刷新 sync cache 失败 channel_id={} message_id={} pts={} error={}",
                    channel_id, message.message_id, pts, e
                );
            }
        } else {
            self.pts_generator.set_pts(channel_id, pts).await;
            warn!(
                "⚠️ SendMessageHandler: SyncService 未就绪，commit 已落库但跳过 Redis sync cache channel_id={} message_id={}",
                channel_id, message.message_id
            );
        }

        // ✨ ServerEvent emit: `system_user.message_received`
        // (spec SERVER_EVENT_DISPATCH_SPEC §11.1 + SYSTEM_USER_SPEC §3)
        //
        // 触发条件（同时满足）：
        //   1. server_event_client 已就绪（即 [server_event] 配置存在）
        //   2. channel.channel_type == Direct
        //   3. channel 对端 user_type == 1 (System User)
        //   4. 发送方 user_type != 1（System User 之间不互推；避免循环 + 误派发）
        //
        // 失败语义：best-effort，warn-log，不阻塞主链路。
        // 注意：server 端**不**查 service_id —— application 端按 system_user_id 1:1 查表。
        if channel.channel_type == crate::model::channel::ChannelType::Direct {
            if let (Some(event_client), Some(user_repo), Some(cache)) = (
                self.server_event_client.clone(),
                self.user_repository.clone(),
                self.cache_manager.clone(),
            ) {
                let peer_id_opt = channel
                    .get_member_ids()
                    .into_iter()
                    .find(|&id| id != from_uid);
                if let Some(peer_id) = peer_id_opt {
                    let sender_id = from_uid;
                    let chan_id_for_emit = channel_id;
                    let msg_id_for_emit = message.message_id;
                    let pts_for_emit = pts as u64;
                    let mtype_for_emit = content_message_type.as_str().to_string();
                    let occurred_at_ms = message_record.created_at.timestamp_millis();
                    tokio::spawn(async move {
                        let sender_t = match crate::rpc::helpers::lookup_user_type(
                            sender_id, &user_repo, &cache,
                        )
                        .await
                        {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(
                                    "system_user.message_received: 查 sender user_type 失败 uid={}: {}",
                                    sender_id, e
                                );
                                return;
                            }
                        };
                        // System User 之间互发不触发（避免循环 + 防呆）。
                        if sender_t == Some(1) {
                            return;
                        }
                        let peer_t = match crate::rpc::helpers::lookup_user_type(
                            peer_id, &user_repo, &cache,
                        )
                        .await
                        {
                            Ok(t) => t,
                            Err(e) => {
                                warn!(
                                    "system_user.message_received: 查 peer user_type 失败 uid={}: {}",
                                    peer_id, e
                                );
                                return;
                            }
                        };
                        if peer_t != Some(1) {
                            return; // 对端不是 System User，跳过。
                        }
                        let event =
                            match crate::server_event::ServerEvent::system_user_message_received(
                                peer_id,
                                sender_id,
                                chan_id_for_emit,
                                msg_id_for_emit,
                                pts_for_emit,
                                mtype_for_emit,
                                occurred_at_ms,
                            ) {
                                Ok(ev) => ev,
                                Err(e) => {
                                    warn!(
                                        "system_user.message_received: payload 序列化失败: {}",
                                        e
                                    );
                                    return;
                                }
                            };
                        if let Err(e) = event_client.send(&event).await {
                            warn!(
                                target: "system_user_event",
                                "system_user.message_received emit 失败 system_user_id={} channel_id={}: {}",
                                peer_id, chan_id_for_emit, e
                            );
                        }
                    });
                }
            }
        }

        // ✨ 发布 MessageCommitted 事件（用于 Push）
        // MVP 阶段：使用全局 EventBus（临时方案）
        if let Some(event_bus) = get_global_event_bus() {
            // 获取接收者 ID（私聊：另一个用户；群聊：所有其他成员）
            let recipient_ids: Vec<u64> =
                if channel.channel_type == crate::model::channel::ChannelType::Direct {
                    // 私聊：获取另一个用户
                    channel
                        .get_member_ids()
                        .into_iter()
                        .filter(|&id| id != from_uid)
                        .collect()
                } else {
                    // 群聊：所有其他成员
                    channel
                        .get_member_ids()
                        .into_iter()
                        .filter(|&id| id != from_uid)
                        .collect()
                };

            // 为每个接收者发布事件（兼容旧逻辑，不指定 device_id）
            for recipient_id in recipient_ids {
                let event = crate::domain::events::DomainEvent::MessageCommitted {
                    message_id: message.message_id,
                    conversation_id: channel_id,
                    sender_id: from_uid,
                    recipient_id,
                    content_preview: content.chars().take(50).collect(),
                    message_type: content_message_type.as_str().to_string(),
                    timestamp: message_record.created_at.timestamp(),
                    device_id: None,
                };

                if let Err(e) = event_bus.publish(event) {
                    warn!(
                        "⚠️ SendMessageHandler: 发布 MessageCommitted 事件失败: {}",
                        e
                    );
                }
            }
        }

        // 5.2. ✨ 处理@提及（使用客户端传递的用户ID列表）
        // 客户端已经选择了用户，mentioned_user_ids 是用户ID列表
        // 不需要解析字符串，直接使用客户端传递的用户ID

        // 5.3. ✨ 处理@提及（使用客户端传递的用户ID列表）
        // 如果是群聊且@全体成员，需要权限检查
        if is_mention_all && channel.channel_type == crate::model::channel::ChannelType::Group {
            // 检查发送者是否有@全体成员的权限（群主或管理员）
            if let Some(member) = channel.members.get(&from_uid) {
                let can_mention_all = matches!(
                    member.role,
                    crate::model::channel::MemberRole::Owner
                        | crate::model::channel::MemberRole::Admin
                );

                if !can_mention_all {
                    warn!(
                        "❌ SendMessageHandler: 用户 {} 无权@全体成员",
                        send_message_request.from_uid
                    );
                    return self
                        .create_error_response(
                            &send_message_request,
                            ErrorCode::PermissionDenied,
                            "无权@全体成员，仅群主和管理员可以",
                        )
                        .await;
                }

                // @全体成员时，获取所有成员ID
                let all_member_ids: Vec<u64> = channel.get_member_ids();
                // 记录@提及（包含所有成员）
                if let Err(e) = self
                    .mention_service
                    .record_mention(
                        message_record.message_id,
                        send_message_request.channel_id,
                        all_member_ids,
                        true,
                    )
                    .await
                {
                    warn!("⚠️ SendMessageHandler: 记录@全体成员失败: {}", e);
                }
            }
        } else if !mentioned_user_ids.is_empty() {
            // 记录@提及（使用客户端传递的用户ID列表）
            // 验证用户ID是否都在频道中
            let valid_user_ids: Vec<u64> = mentioned_user_ids
                .iter()
                .filter(|&&user_id| channel.members.contains_key(&user_id))
                .cloned()
                .collect();

            if valid_user_ids.len() != mentioned_user_ids.len() {
                warn!("⚠️ SendMessageHandler: 部分@的用户不在频道中，已过滤");
            }

            if !valid_user_ids.is_empty() {
                if let Err(e) = self
                    .mention_service
                    .record_mention(
                        message_record.message_id,
                        send_message_request.channel_id,
                        valid_user_ids,
                        false,
                    )
                    .await
                {
                    warn!("⚠️ SendMessageHandler: 记录@提及失败: {}", e);
                }
            }
        }

        // 5.1. 更新频道最后消息信息
        if let Err(e) = self
            .channel_service
            .update_last_message(channel_id, message_record.message_id)
            .await
        {
            warn!("⚠️ SendMessageHandler: 更新频道最后消息信息失败: {}", e);
        }

        // 5.5. channel last preview is derived client-side from local message table.
        // Server keeps stable anchor only (last_message_id/timestamp) via update_last_message.

        // 6. Use the same DB-authoritative unread projection as sync/submit.
        // The previous Redis-only write made live badges appear, then reset to
        // zero when a browser refresh rebuilt channels from PostgreSQL.
        if let Some(sync_service) = get_global_sync_service() {
            sync_service
                .enqueue_unread_increment(channel.id, from_uid, 1)
                .await;
        } else {
            // Startup should install the global SyncService before accepting
            // traffic. Fail visibly instead of recreating a cache-only split.
            warn!(
                channel_id = channel.id,
                sender_id = from_uid,
                "SyncService unavailable; unread projection was not queued"
            );
        }

        // 7. 先创建成功响应（不阻塞客户端）
        let response = self
            .create_success_response(&send_message_request, &message_record, pts as u64)
            .await;

        crate::infra::metrics::record_message_sent();

        // 8. Immediate low-latency attempt. The durable outbox worker uses the
        // same lease claim and is the crash/retry backstop, so this cannot
        // double-fanout.
        if let Some(event_id) = event_id {
            let delivery = self.delivery_service.clone();
            tokio::spawn(async move {
                if let Err(error) = delivery.dispatch_event(event_id).await {
                    warn!(event_id, %error, "wire immediate dispatch failed; worker will retry");
                }
            });
        }

        response
    }

    fn name(&self) -> &'static str {
        "SendMessageHandler"
    }
}

impl SendMessageHandler {
    /// 解析 payload 为 content + metadata + reply_to_message_id + mentioned_user_ids + message_source
    /// 消息类型由协议层 SendMessageRequest.message_type（u32）提供，不在此解析。
    /// 二进制判定：非 UTF-8，或含 C0 控制字节（保留用户可合法输入的 \t \n \r）。真实聊天文本
    /// 不会命中；FlatBuffers 头/表必含 NUL 或小整数字节必命中。与 TS SDK
    /// `decodePlainTextPayload`（cache/types.ts）语义一致。
    fn payload_looks_binary(payload: &[u8]) -> bool {
        match std::str::from_utf8(payload) {
            Err(_) => true,
            Ok(s) => s
                .bytes()
                .any(|b| b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r'),
        }
    }

    /// Payload 格式见 protocol 层 MessagePayloadEnvelope。
    fn parse_payload(
        payload: &[u8],
    ) -> crate::Result<(
        String,
        Option<String>,
        Option<String>,
        Vec<u64>,
        Option<crate::model::privacy::FriendRequestSource>,
    )> {
        // Wire format is FlatBuffers MessagePayloadEnvelope (zero-copy
        // typed metadata union). Downstream code still wants metadata as
        // an opaque JSON string (validation re-parses per content type),
        // so we serialize the typed inner metadata back to JSON without
        // the discriminator tag — matching the legacy on-disk shape.
        use privchat_protocol::{MessagePayloadEnvelope, MessageSource};

        let parsed: MessagePayloadEnvelope =
            match privchat_protocol::decode_message::<MessagePayloadEnvelope>(payload) {
                Ok(v) => v,
                Err(_) => {
                    // 二进制守卫（生产乱码事故回归；对齐 TS decodePlainTextPayload）：FlatBuffers/未知
                    // 二进制在 decode 失败时绝不能 lossy-stringify 落库——那会存成「不可见控制符+字符串
                    // 长度字节+正文」的永久脏数据。非 UTF-8 或含 C0 控制符（\t\n\r 除外）= 二进制 → 拒绝。
                    if Self::payload_looks_binary(payload) {
                        return Err(crate::ServerError::Validation(
                            "消息负载既非合法 FlatBuffers envelope 也非文本".to_string(),
                        )
                        .into());
                    }
                    // fallback: 纯文本模式（旧客户端 / 测试可能直接传 utf-8）
                    let text = String::from_utf8_lossy(payload).replace('\u{0}', "");
                    if let Some(normalized) = Self::parse_legacy_json_payload(&text) {
                        return Ok(normalized);
                    }
                    return Ok((text, None, None, Vec::new(), None));
                }
            };

        // content: envelope 已成功解码，以 parsed.content 为准。媒体消息（图片/语音/
        // 视频/文件）无 caption 时 content 就是空串，媒体信息全在 metadata 里 —— 绝不能
        // 把整个 FlatBuffers 二进制 payload 当字符串塞进 content：payload 里的 0x00 会让
        // Postgres 的 UTF8 text 列直接拒绝（invalid byte sequence for encoding "UTF8": 0x00），
        // 导致无 caption 的图片消息 100% 保存失败。纯文本（非 envelope）走上面 decode 失败分支。
        // 顺带剥掉任何 NUL：text 列无法存储 0x00。
        let content = parsed.content.replace('\u{0}', "");

        // metadata: 把 typed 枚举转回 inner JSON shape（不带 type 标签）
        let metadata = parsed.metadata.as_ref().map(|m| {
            serde_json::to_string(&m.to_inner_json_value()).unwrap_or_else(|_| "{}".to_string())
        });

        // reply_to_message_id: u64 → Option<String> for downstream API contract
        let reply_to_message_id = parsed.reply_to_message_id.map(|n| n.to_string());

        // message_source: 从 MessageSource 转为 FriendRequestSource 枚举
        let message_source =
            parsed
                .message_source
                .and_then(|src: MessageSource| match src.source_type.as_str() {
                    "search" => src.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::Search {
                            search_session_id: id,
                        }
                    }),
                    "group" => src.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::Group { group_id: id }
                    }),
                    "card_share" => src.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::CardShare { share_id: id }
                    }),
                    "qrcode" => Some(crate::model::privacy::FriendRequestSource::Qrcode {
                        qrcode: src.source_id,
                    }),
                    "phone" => Some(crate::model::privacy::FriendRequestSource::Phone {
                        phone: src.source_id,
                    }),
                    _ => None,
                });

        Ok((
            content,
            metadata,
            reply_to_message_id,
            parsed.mentioned_user_ids,
            message_source,
        ))
    }

    fn parse_legacy_json_payload(
        text: &str,
    ) -> Option<(
        String,
        Option<String>,
        Option<String>,
        Vec<u64>,
        Option<crate::model::privacy::FriendRequestSource>,
    )> {
        use privchat_protocol::message::LocalMessagePayloadEnvelope;

        let value = serde_json::from_str::<serde_json::Value>(text.trim()).ok()?;
        let object = value.as_object()?;
        object.get("content")?.as_str()?;
        let is_envelope = [
            "metadata",
            "reply_to_message_id",
            "mentioned_user_ids",
            "message_source",
        ]
        .iter()
        .any(|key| object.contains_key(*key));
        if !is_envelope {
            return None;
        }

        let envelope = serde_json::from_value::<LocalMessagePayloadEnvelope>(value).ok()?;
        let metadata = envelope
            .metadata
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok());
        let message_source =
            envelope
                .message_source
                .and_then(|source| match source.source_type.as_str() {
                    "search" => source.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::Search {
                            search_session_id: id,
                        }
                    }),
                    "group" => source.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::Group { group_id: id }
                    }),
                    "card_share" => source.source_id.parse::<u64>().ok().map(|id| {
                        crate::model::privacy::FriendRequestSource::CardShare { share_id: id }
                    }),
                    "qrcode" => Some(crate::model::privacy::FriendRequestSource::Qrcode {
                        qrcode: source.source_id,
                    }),
                    "phone" => Some(crate::model::privacy::FriendRequestSource::Phone {
                        phone: source.source_id,
                    }),
                    _ => None,
                });

        Some((
            envelope.content.replace('\u{0}', ""),
            metadata,
            envelope.reply_to_message_id,
            envelope.mentioned_user_ids.unwrap_or_default(),
            message_source,
        ))
    }

    /// 验证消息类型和 metadata（按 protocol 层 ContentMessageType 与对应 metadata 结构体）
    async fn validate_message_metadata(
        &self,
        message_type: privchat_protocol::ContentMessageType,
        metadata_json: &Option<String>,
        sender_id: u64,
    ) -> crate::Result<()> {
        let metadata_json = match metadata_json {
            Some(m) => m,
            None => {
                if matches!(
                    message_type,
                    privchat_protocol::ContentMessageType::Text
                        | privchat_protocol::ContentMessageType::System
                ) {
                    return Ok(());
                }
                return Err(crate::error::ServerError::Validation(format!(
                    "消息类型 {} 需要 metadata",
                    message_type.as_str()
                )));
            }
        };

        let metadata: Value = serde_json::from_str(metadata_json).map_err(|e| {
            crate::error::ServerError::Validation(format!("metadata 不是有效的 JSON: {}", e))
        })?;

        match message_type {
            privchat_protocol::ContentMessageType::Text
            | privchat_protocol::ContentMessageType::System
            // RP-12：资金消息卡片的 content 是订单引用 JSON，无 file/location metadata 需校验。
            | privchat_protocol::ContentMessageType::RedPacket
            | privchat_protocol::ContentMessageType::MoneyTransfer => Ok(()),
            privchat_protocol::ContentMessageType::Image => {
                self.validate_file_metadata(&metadata, "image", sender_id)
                    .await
            }
            privchat_protocol::ContentMessageType::Video => {
                self.validate_file_metadata(&metadata, "video", sender_id)
                    .await
            }
            privchat_protocol::ContentMessageType::Voice => {
                self.validate_file_metadata(&metadata, "voice", sender_id)
                    .await
            }
            privchat_protocol::ContentMessageType::File => {
                self.validate_file_metadata(&metadata, "file", sender_id)
                    .await
            }
            privchat_protocol::ContentMessageType::Location => {
                self.validate_location_metadata(&metadata).await
            }
            privchat_protocol::ContentMessageType::ContactCard => {
                self.validate_contact_card_metadata(&metadata).await
            }
            privchat_protocol::ContentMessageType::Sticker => {
                self.validate_sticker_metadata(&metadata).await
            }
            privchat_protocol::ContentMessageType::Forward => {
                self.validate_forward_metadata(&metadata).await
            }
            privchat_protocol::ContentMessageType::Link => {
                self.validate_link_metadata(&metadata).await
            }
        }
    }

    /// 从消息 metadata 提取所有附件 file_id（原文件 + 缩略图），用于 file→message 绑定授权。
    /// 覆盖 image/video/voice/file 的 `file_id` 与 video/link/location 的 `thumbnail_file_id`。
    /// 数字或字符串均兼容；去重；忽略 0/缺失。
    fn extract_attachment_file_ids(metadata: &serde_json::Value) -> Vec<u64> {
        let mut ids = Vec::new();
        for key in ["file_id", "thumbnail_file_id"] {
            if let Some(v) = metadata.get(key) {
                let id = v
                    .as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()));
                if let Some(id) = id {
                    if id > 0 && !ids.contains(&id) {
                        ids.push(id);
                    }
                }
            }
        }
        ids
    }

    /// 验证文件类型消息的 metadata（image/video/audio/file）
    async fn validate_file_metadata(
        &self,
        metadata: &Value,
        file_type_name: &str,
        sender_id: u64,
    ) -> crate::Result<()> {
        // 验证 file_id 字段存在（扁平结构，支持数字或字符串）
        let file_id = metadata
            .get("file_id")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .ok_or_else(|| {
                crate::error::ServerError::Validation(format!(
                    "{} 消息缺少或无效的 metadata.file_id",
                    file_type_name
                ))
            })?;

        match self.file_service.get_file_metadata(file_id).await {
            Ok(Some(file_metadata)) => {
                // 验证文件所有权
                if file_metadata.uploader_id != sender_id {
                    return Err(crate::error::ServerError::Authorization(format!(
                        "文件 {} 不属于发送者 {}",
                        file_id, sender_id
                    )));
                }
                info!(
                    "✅ 文件消息验证通过: type={}, file_id={}, uploader={}",
                    file_type_name, file_id, file_metadata.uploader_id
                );
                Ok(())
            }
            Ok(None) => Err(crate::error::ServerError::NotFound(format!(
                "文件 {} 不存在",
                file_id
            ))),
            Err(e) => {
                warn!("⚠️ 验证文件 {} 时出错: {}", file_id, e);
                Err(crate::error::ServerError::Internal(e.to_string()))
            }
        }
    }

    /// 验证位置消息 metadata
    async fn validate_location_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证必需字段（扁平结构）
        metadata
            .get("latitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "location 消息缺少有效的 latitude".to_string(),
                )
            })?;

        metadata
            .get("longitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "location 消息缺少有效的 longitude".to_string(),
                )
            })?;

        Ok(())
    }

    /// 验证名片消息 metadata
    async fn validate_contact_card_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证 user_id（扁平结构），接受 u64 或字符串类型
        metadata
            .get("user_id")
            .and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok()))
            })
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "contact_card 消息缺少 metadata.user_id (must be u64)".to_string(),
                )
            })?;

        Ok(())
    }

    /// 验证表情包消息 metadata
    async fn validate_sticker_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证必需字段（扁平结构）
        metadata
            .get("sticker_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "sticker 消息缺少 metadata.sticker_id".to_string(),
                )
            })?;

        metadata
            .get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "sticker 消息缺少 metadata.image_url".to_string(),
                )
            })?;

        Ok(())
    }

    /// 验证网址预览消息 metadata
    ///
    /// 仅强制 `url` 必填；`title` / `description` / `thumbnail_file_id` 由 SDK 应用层预览
    /// 回调填充，服务端不参与抓取，缺失时按空白预览渲染。
    async fn validate_link_metadata(&self, metadata: &Value) -> crate::Result<()> {
        let url = metadata
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                crate::error::ServerError::Validation("link 消息缺少 metadata.url".to_string())
            })?;

        if url.trim().is_empty() {
            return Err(crate::error::ServerError::Validation(
                "link 消息的 metadata.url 不能为空".to_string(),
            ));
        }

        Ok(())
    }

    /// 验证转发消息 metadata
    async fn validate_forward_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证 messages 数组（扁平结构）
        let messages = metadata
            .get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                crate::error::ServerError::Validation(
                    "forward 消息缺少 metadata.messages 数组".to_string(),
                )
            })?;

        if messages.is_empty() {
            return Err(crate::error::ServerError::Validation(
                "forward 消息的 messages 数组不能为空".to_string(),
            ));
        }

        Ok(())
    }

    /// 获取或创建私聊频道（已废弃，channel_id 现在直接是 u64）
    /// 此方法保留用于向后兼容，但不应被调用
    async fn get_or_create_private_channel(
        &self,
        send_message_request: &privchat_protocol::protocol::SendMessageRequest,
    ) -> Result<crate::model::channel::Channel> {
        // channel_id 现在直接是 u64，直接获取频道
        self.channel_service
            .get_channel(&send_message_request.channel_id)
            .await
            .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))
    }

    /// 创建成功响应
    async fn create_success_response(
        &self,
        request: &privchat_protocol::protocol::SendMessageRequest,
        message_record: &crate::service::message_history_service::MessageHistoryRecord,
        pts: u64,
    ) -> Result<Option<Vec<u8>>> {
        // message_id 现在已经是 u64 类型，直接使用
        let response = privchat_protocol::protocol::SendMessageResponse {
            client_seq: request.client_seq,
            server_message_id: message_record.message_id,
            message_seq: pts as u32,
            reason_code: 0, // 成功
        };

        let response_bytes = privchat_protocol::encode_message(&response)
            .map_err(|e| ServerError::Protocol(format!("编码响应失败: {}", e)))?;

        info!(
            "✅ SendMessageHandler: 消息发送成功 - 消息ID: {}, 序号: {}",
            message_record.message_id, message_record.seq
        );

        Ok(Some(response_bytes))
    }

    /// 创建重复消息响应（幂等性处理）
    async fn create_duplicate_response(
        &self,
        request: &privchat_protocol::protocol::SendMessageRequest,
    ) -> Result<Option<Vec<u8>>> {
        // 对于重复消息，我们需要返回一个成功响应，但不处理消息
        // 这里我们需要查询之前处理过的消息记录，但由于去重服务只存储了 (user_id, local_message_id)，
        // 我们无法直接获取之前的 message_id 和 message_seq
        // 为了简化，我们返回一个特殊的响应码，或者返回一个默认的成功响应

        // 注意：这里返回的 message_id 和 message_seq 可能不准确，因为这是重复消息
        // 实际应用中，可以考虑在去重服务中存储完整的消息记录
        let response = privchat_protocol::protocol::SendMessageResponse {
            client_seq: request.client_seq,
            server_message_id: 0, // 重复消息，无法获取真实ID
            message_seq: 0,
            reason_code: 0, // 成功（幂等性）
        };

        let response_bytes = privchat_protocol::encode_message(&response)
            .map_err(|e| ServerError::Protocol(format!("编码响应失败: {}", e)))?;

        info!(
            "🔄 SendMessageHandler: 重复消息已忽略 - 用户: {}, local_message_id: {}",
            request.from_uid, request.local_message_id
        );

        Ok(Some(response_bytes))
    }

    /// 创建错误响应
    async fn create_error_response(
        &self,
        request: &privchat_protocol::protocol::SendMessageRequest,
        error_code: ErrorCode,
        error_message: &str,
    ) -> Result<Option<Vec<u8>>> {
        let response = privchat_protocol::protocol::SendMessageResponse {
            client_seq: request.client_seq,
            server_message_id: 0, // 错误时使用0
            message_seq: 0,
            reason_code: error_code.code(),
        };

        let response_bytes = privchat_protocol::encode_message(&response)
            .map_err(|e| ServerError::Protocol(format!("编码错误响应失败: {}", e)))?;

        warn!(
            "❌ SendMessageHandler: 消息发送失败 - 错误码: {}, 错误信息: {}",
            error_code, error_message
        );

        Ok(Some(response_bytes))
    }

    /// 生成私聊会话ID（使用UUID v5，确保确定性）
    fn generate_private_chat_id(&self, user1: &str, user2: &str) -> String {
        use uuid::Uuid;

        match (Uuid::parse_str(user1), Uuid::parse_str(user2)) {
            (Ok(user1_uuid), Ok(user2_uuid)) => {
                // 使用 UUID v5 生成确定性的会话 ID（与 friend/accept.rs 一致）
                let namespace = Uuid::parse_str("6ba7b810-9dad-11d1-80b4-00c04fd430c8").unwrap(); // DNS namespace
                let (id1, id2) = if user1_uuid < user2_uuid {
                    (user1_uuid, user2_uuid)
                } else {
                    (user2_uuid, user1_uuid)
                };
                Uuid::new_v5(&namespace, format!("{}:{}", id1, id2).as_bytes()).to_string()
            }
            _ => {
                // 如果用户ID不是UUID格式，使用简单的哈希
                use std::collections::hash_map::DefaultHasher;
                use std::hash::Hash;
                let mut hasher = DefaultHasher::new();
                let (u1, u2) = if user1 < user2 {
                    (user1, user2)
                } else {
                    (user2, user1)
                };
                format!("{}:{}", u1, u2).hash(&mut hasher);
                Uuid::new_v4().to_string() // 临时方案，应该确保用户ID是UUID格式
            }
        }
    }

    /// 确保会话存在并将成员加入会话
    async fn ensure_channel_and_members(
        &self,
        channel_id: u64,
        channel: &crate::model::channel::Channel,
    ) {
        use crate::model::channel::{ChannelType, CreateChannelRequest};

        // 获取频道成员列表
        let member_ids: Vec<u64> = channel.members.keys().cloned().collect();

        if member_ids.is_empty() {
            warn!(
                "⚠️ SendMessageHandler: 频道 {} 没有成员，跳过会话创建",
                channel_id
            );
            return;
        }

        // 检查会话是否存在
        let channel_exists = self.channel_service.get_channel(&channel_id).await.is_ok();

        if channel_exists {
            // 会话已存在，无需重复创建或加入成员
            return;
        }

        // 会话不存在，自动创建会话
        let channel_type = match channel.channel_type {
            crate::model::channel::ChannelType::Direct => ChannelType::Direct,
            crate::model::channel::ChannelType::Group => ChannelType::Group,
            crate::model::channel::ChannelType::Room => ChannelType::Group,
        };

        // 使用第一个成员作为创建者（通常是频道的owner）
        let creator_id = member_ids.first().copied().unwrap_or(0);

        let create_request = CreateChannelRequest {
            channel_type,
            name: None,
            description: None,
            member_ids: member_ids.iter().skip(1).copied().collect(),
            is_public: Some(false),
            max_members: None,
        };

        match self
            .channel_service
            .create_channel_with_id(channel_id, creator_id, create_request)
            .await
        {
            Ok(_) => {
                info!(
                    "✅ SendMessageHandler: 为频道 {} 自动创建了会话",
                    channel_id
                );
            }
            Err(e) => {
                warn!(
                    "⚠️ SendMessageHandler: 为频道 {} 创建会话失败: {}",
                    channel_id, e
                );
            }
        }
    }
}

#[cfg(test)]
mod attachment_bind_tests {
    use super::SendMessageHandler;
    use serde_json::json;

    // 原图 + 缩略图都要被提取（缩略图同等绑定 → 接收方能授权下载缩略图）
    #[test]
    fn extracts_file_and_thumbnail() {
        let meta = json!({ "file_id": 100u64, "thumbnail_file_id": 200u64, "width": 800 });
        let ids = SendMessageHandler::extract_attachment_file_ids(&meta);
        assert!(ids.contains(&100));
        assert!(ids.contains(&200));
        assert_eq!(ids.len(), 2);
    }

    // 字符串形式 id 兼容
    #[test]
    fn accepts_string_ids() {
        let meta = json!({ "file_id": "300", "thumbnail_file_id": "0" });
        let ids = SendMessageHandler::extract_attachment_file_ids(&meta);
        assert_eq!(ids, vec![300]); // 0 被忽略
    }

    // 文本消息无附件 → 空
    #[test]
    fn text_message_has_no_attachments() {
        let meta = json!({ "text": "hello" });
        assert!(SendMessageHandler::extract_attachment_file_ids(&meta).is_empty());
    }

    // 去重：原图与缩略图同 id 只保留一个
    #[test]
    fn dedups_same_id() {
        let meta = json!({ "file_id": 5u64, "thumbnail_file_id": 5u64 });
        assert_eq!(
            SendMessageHandler::extract_attachment_file_ids(&meta),
            vec![5]
        );
    }

    // CODEX-8 复审 P0#3/P1#4：sender/device 认证权威 + 冒充/缺失拒绝（纯函数回归）。
    use privchat_protocol::error_code::ErrorCode;
    fn sess(uid: &str, dev: &str) -> Option<(String, String)> {
        Some((uid.to_string(), dev.to_string()))
    }

    #[test]
    fn sender_valid_uses_session_identity() {
        // payload from_uid=0（未提供）→ 用认证 uid+device
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(sess("42", "devA"), 0),
            Ok((42, "devA".to_string()))
        );
        // payload from_uid 与认证一致 → 通过
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(sess("42", "devA"), 42),
            Ok((42, "devA".to_string()))
        );
    }

    #[test]
    fn sender_from_uid_mismatch_rejected() {
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(sess("42", "devA"), 99),
            Err(ErrorCode::PermissionDenied)
        );
    }

    #[test]
    fn sender_missing_session_rejected() {
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(None, 42),
            Err(ErrorCode::AuthRequired)
        );
    }

    #[test]
    fn sender_empty_device_rejected() {
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(sess("42", ""), 0),
            Err(ErrorCode::AuthRequired)
        );
    }

    #[test]
    fn sender_unparseable_uid_rejected() {
        assert_eq!(
            SendMessageHandler::resolve_authenticated_sender(sess("not-a-number", "devA"), 0),
            Err(ErrorCode::AuthRequired)
        );
    }
}

#[cfg(test)]
mod payload_normalization_tests {
    use super::SendMessageHandler;

    #[test]
    fn legacy_json_envelope_is_normalized_before_persistence() {
        let payload = serde_json::json!({
            "content": "归一化后的正文",
            "mentioned_user_ids": [],
            "reply_to_message_id": "600997771041832960"
        })
        .to_string();
        let (content, metadata, reply, mentions, _) =
            SendMessageHandler::parse_payload(payload.as_bytes()).expect("parse legacy payload");
        assert_eq!(content, "归一化后的正文");
        assert!(metadata.is_none());
        assert_eq!(reply.as_deref(), Some("600997771041832960"));
        assert!(mentions.is_empty());
    }

    #[test]
    fn user_authored_json_with_only_content_remains_text() {
        let payload = r#"{"content":"literal user JSON"}"#;
        let (content, metadata, reply, mentions, _) =
            SendMessageHandler::parse_payload(payload.as_bytes()).expect("parse literal json");
        assert_eq!(content, payload);
        assert!(metadata.is_none());
        assert!(reply.is_none());
        assert!(mentions.is_empty());
    }

    // 生产乱码事故回归（web FB envelope × 版本偏斜）：FlatBuffers 二进制在 decode 失败时
    // 绝不能被 lossy-stringify 落库成「不可见控制符+长度字节+正文」——必须拒绝（对齐 TS
    // decodePlainTextPayload 的二进制守卫）。
    #[test]
    fn undecodable_binary_payload_rejected_not_mojibaked() {
        // 合法 envelope → 损坏 root offset 使 verifier 失败，但字节仍是典型 FB 二进制（含 C0/NUL）。
        let mut bytes = privchat_protocol::encode_message(&privchat_protocol::MessagePayloadEnvelope {
            content: "大家记住一句话，天下没有白吃的苦，没有白走的路！".to_string(),
            ..Default::default()
        })
        .expect("encode envelope");
        bytes[0] = 0xF0; bytes[1] = 0xFF; bytes[2] = 0xFF; bytes[3] = 0xFF; // root offset 指向界外
        assert!(
            SendMessageHandler::parse_payload(&bytes).is_err(),
            "binary garbage must be rejected, not stored as mojibake text"
        );
    }

    // 二进制守卫不能误伤：用户可合法输入的控制符（\t \n \r）仍按纯文本接受。
    #[test]
    fn plain_text_with_newline_still_accepted() {
        let payload = "第一行\n\t第二行\r\n第三行";
        let (content, ..) =
            SendMessageHandler::parse_payload(payload.as_bytes()).expect("plain text with newline");
        assert_eq!(content, payload);
    }
}
