use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, debug, warn, error};
use msgtrans::{SessionId, packet::Packet};
use serde_json::Value;
use crate::{Result};
use crate::context::RequestContext;
use crate::handler::MessageHandler;
use crate::service::message_history_service::MessageHistoryService;
use crate::model::channel::{UserId, MessageId};
use crate::error::ServerError;
use crate::session::SessionManager;
use crate::infra::SessionManager as AuthSessionManager;
use crate::model::pts::{PtsGenerator, UserMessageIndex};
use crate::service::{OfflineQueueService, UnreadCountService, MessageDedupService};
use crate::repository::{MessageRepository, PgMessageRepository};

/// 发送消息处理器
pub struct SendMessageHandler {
    // channel_service 已合并到 channel_service
    /// 消息历史服务
    message_history_service: Arc<MessageHistoryService>,
    /// 会话管理器
    session_manager: Arc<SessionManager>,
    /// 文件服务（用于验证 file_id）
    file_service: Arc<crate::service::FileService>,
    /// 会话服务（用于更新会话列表）
    channel_service: Arc<crate::service::ChannelService>,
    /// 传输层服务器（可选，运行时设置）
    transport: Arc<RwLock<Option<Arc<msgtrans::transport::TransportServer>>>>,
    /// 消息路由器（用于离线消息队列）
    message_router: Arc<crate::infra::MessageRouter>,
    /// 黑名单服务（用于拦截被拉黑用户的消息）
    blacklist_service: Arc<crate::service::BlacklistService>,
    /// ✨ pts 生成器
    pts_generator: Arc<PtsGenerator>,
    /// ✨ 用户消息索引（pts -> message_id 映射）
    user_message_index: Arc<UserMessageIndex>,
    /// ✨ 离线消息队列服务
    offline_queue_service: Arc<OfflineQueueService>,
    /// ✨ 未读计数服务
    unread_count_service: Arc<UnreadCountService>,
    /// ✨ 消息去重服务
    message_dedup_service: Arc<MessageDedupService>,
    /// ✨ 隐私服务（用于检查非好友消息权限）
    privacy_service: Arc<crate::service::PrivacyService>,
    /// ✨ 好友服务（用于检查好友关系）
    friend_service: Arc<crate::service::FriendService>,
    /// ✨ @提及服务（用于处理@提及功能）
    mention_service: Arc<crate::service::MentionService>,
    /// ✨ 消息仓库（PostgreSQL）
    message_repository: Arc<PgMessageRepository>,
    /// ✨ 事件总线（用于发布 Domain Events）
    event_bus: Option<Arc<crate::infra::EventBus>>,
    /// ✨ Phase 3.5: 用户设备仓库（用于推送设备查询）
    user_device_repo: Option<Arc<crate::repository::UserDeviceRepository>>,
    /// 认证会话管理器（用于 READY 推送闸门）
    auth_session_manager: Arc<AuthSessionManager>,
}

// 临时全局 EventBus（MVP 阶段简化方案）
// TODO: 未来应该通过依赖注入传递
static mut GLOBAL_EVENT_BUS: Option<Arc<crate::infra::EventBus>> = None;

pub fn set_global_event_bus(event_bus: Arc<crate::infra::EventBus>) {
    unsafe {
        GLOBAL_EVENT_BUS = Some(event_bus);
    }
}

/// 获取全局 EventBus（供其他模块使用）
pub fn get_global_event_bus() -> Option<Arc<crate::infra::EventBus>> {
    unsafe {
        GLOBAL_EVENT_BUS.clone()
    }
}

impl SendMessageHandler {
    pub fn new(
        // channel_service 已合并到 channel_service
        message_history_service: Arc<MessageHistoryService>,
        session_manager: Arc<SessionManager>,
        file_service: Arc<crate::service::FileService>,
        channel_service: Arc<crate::service::ChannelService>,
        message_router: Arc<crate::infra::MessageRouter>,
        blacklist_service: Arc<crate::service::BlacklistService>,
        pts_generator: Arc<PtsGenerator>,
        user_message_index: Arc<UserMessageIndex>,
        offline_queue_service: Arc<OfflineQueueService>,
        unread_count_service: Arc<UnreadCountService>,
        message_dedup_service: Arc<MessageDedupService>,
        privacy_service: Arc<crate::service::PrivacyService>,
        friend_service: Arc<crate::service::FriendService>,
        mention_service: Arc<crate::service::MentionService>,
        message_repository: Arc<PgMessageRepository>,
        auth_session_manager: Arc<AuthSessionManager>,
        user_device_repo: Option<Arc<crate::repository::UserDeviceRepository>>,  // ✨ Phase 3.5
    ) -> Self {
        Self {
            // channel_service 已合并到 channel_service
            message_history_service,
            session_manager,
            file_service,
            channel_service,
            transport: Arc::new(RwLock::new(None)),
            message_router,
            blacklist_service,
            pts_generator,
            user_message_index,
            offline_queue_service,
            unread_count_service,
            message_dedup_service,
            privacy_service,
            friend_service,
            mention_service,
            message_repository,
            event_bus: None,
            auth_session_manager,
            user_device_repo,  // ✨ Phase 3.5
        }
    }
    
    /// 设置事件总线（在服务器启动后调用）
    pub fn set_event_bus(&mut self, event_bus: Arc<crate::infra::EventBus>) {
        self.event_bus = Some(event_bus);
    }
    
    /// 设置传输层服务器（在服务器启动后调用）
    pub async fn set_transport(&self, transport: Arc<msgtrans::transport::TransportServer>) {
        *self.transport.write().await = Some(transport);
    }
}

#[async_trait]
impl MessageHandler for SendMessageHandler {
    async fn handle(&self, context: RequestContext) -> Result<Option<Vec<u8>>> {
        info!("📢 SendMessageHandler: 处理来自会话 {} 的消息发送请求", context.session_id);
        
        // 1. 解析发送请求
        let send_message_request: privchat_protocol::protocol::SendMessageRequest = privchat_protocol::decode_message(&context.data)
            .map_err(|e| ServerError::Protocol(format!("解码发送请求失败: {}", e)))?;
        
        info!("📤 SendMessageHandler: 处理发送请求 - 用户: {}, 频道: {}, 内容长度: {}", 
              send_message_request.from_uid, send_message_request.channel_id, send_message_request.payload.len());
        
        // from_uid 和 channel_id 现在已经是 u64 类型
        let from_uid = send_message_request.from_uid;
        let channel_id = send_message_request.channel_id;
        
        // ✨ 1.4. 防御性检查：拒绝无效的 channel_id
        if channel_id == 0 {
            error!("❌ SendMessageHandler: 无效的 channel_id: 0（用户: {}）", from_uid);
            return self.create_error_response(&send_message_request, 1, "无效的频道ID").await;
        }
        
        // 1.5. 检查消息去重（基于 local_message_id）
        if self.message_dedup_service.is_duplicate(from_uid, send_message_request.local_message_id).await {
            warn!("🔄 SendMessageHandler: 检测到重复消息 - 用户: {}, local_message_id: {}", 
                  send_message_request.from_uid, send_message_request.local_message_id);
            // 返回成功响应，但不处理消息（幂等性）
            return self.create_duplicate_response(&send_message_request).await;
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
                                    db_channel.direct_user1_id.map(|id| id == from_uid).unwrap_or(false)
                                        || db_channel.direct_user2_id.map(|id| id == from_uid).unwrap_or(false)
                                }
                            }
                            crate::model::channel::ChannelType::Group => {
                                // 群聊：检查成员列表
                                // ✨ 优先检查内存中的成员列表，如果为空则从数据库查询参与者
                                if !channel.members.is_empty() {
                                    channel.members.contains_key(&from_uid)
                                } else {
                                    // 从数据库查询参与者
                                    if let Ok(participants) = self.channel_service.get_channel_participants(channel_id).await {
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
                                    info!("🔧 SendMessageHandler: 用户 {} 不在私聊频道 {} 的成员列表中，但从数据库会话中发现是参与者，自动添加", 
                                          from_uid, channel_id);
                                    crate::model::channel::MemberRole::Member
                                }
                                crate::model::channel::ChannelType::Group => {
                                    // 从数据库查询用户的角色
                                    let participant_role = if channel.members.is_empty() {
                                        // 如果内存中的成员列表为空，从数据库查询
                                        if let Ok(participants) = self.channel_service.get_channel_participants(channel_id).await {
                                            participants.iter()
                                                .find(|p| p.user_id == from_uid)
                                                .map(|p| p.role.clone())
                                                .unwrap_or(crate::model::channel::MemberRole::Member)
                                        } else {
                                            crate::model::channel::MemberRole::Member
                                        }
                                    } else {
                                        channel.members.get(&from_uid)
                                            .map(|m| m.role.clone())
                                            .unwrap_or(crate::model::channel::MemberRole::Member)
                                    };
                                    
                                    info!("🔧 SendMessageHandler: 用户 {} 不在群聊频道 {} 的成员列表中，但从数据库会话中发现是参与者（角色: {:?}），自动添加", 
                                          from_uid, channel_id, participant_role);
                                    
                                    match participant_role {
                                        crate::model::channel::MemberRole::Owner => crate::model::channel::MemberRole::Owner,
                                        crate::model::channel::MemberRole::Admin => crate::model::channel::MemberRole::Admin,
                                        crate::model::channel::MemberRole::Member => crate::model::channel::MemberRole::Member,
                                    }
                                }
                                _ => crate::model::channel::MemberRole::Member,
                            };
                            
                            if let Err(e) = self.channel_service.join_channel(
                                channel_id, 
                                from_uid, 
                                Some(role)
                            ).await {
                                warn!("❌ SendMessageHandler: 添加用户到频道失败: {}", e);
                                return self.create_error_response(&send_message_request, 4, "无法加入频道").await;
                            }
                            // 重新获取频道（包含新成员）
                            self.channel_service.get_channel(&channel_id).await
                                .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))?
                        } else {
                            // 用户不是参与者，拒绝访问
                            warn!("❌ SendMessageHandler: 用户 {} 不是频道 {} 的参与者", 
                                  from_uid, channel_id);
                            return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
                        }
                    } else {
                        // 会话不存在，拒绝访问
                        warn!("❌ SendMessageHandler: 会话 {} 不存在", channel_id);
                        return self.create_error_response(&send_message_request, 1, "会话不存在").await;
                    }
                } else {
                    channel
                }
            },
            Err(_) => {
                // ✨ 频道不存在，尝试恢复或创建
                // 先尝试从数据库会话中恢复频道（channel_id可能是UUID格式的channel_id）
                if let Ok(channel) = self.channel_service.get_channel(&send_message_request.channel_id).await {
                    // 会话存在，尝试恢复频道
                    info!("🔧 SendMessageHandler: 频道 {} 不存在但会话存在，尝试恢复频道", send_message_request.channel_id);
                    
                    // 根据会话类型创建频道
                    match channel.channel_type {
                        crate::model::channel::ChannelType::Direct => {
                            // 私聊：从 direct_user1_id 和 direct_user2_id 获取两个用户ID
                            if let (Some(user1_id), Some(user2_id)) = (channel.direct_user1_id, channel.direct_user2_id) {
                                // ✨ 验证发送者是否是会话的参与者
                                if from_uid != user1_id && from_uid != user2_id {
                                    warn!("❌ SendMessageHandler: 用户 {} 不是私聊会话 {} 的参与者", 
                                          from_uid, channel_id);
                                    return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
                                }
                                
                                // 使用会话ID创建私聊频道
                                if let Err(e) = self.channel_service.create_private_chat_with_id(
                                    user1_id,
                                    user2_id,
                                    channel_id,
                                ).await {
                                    warn!("❌ SendMessageHandler: 恢复私聊频道失败: {}", e);
                                    return self.create_error_response(&send_message_request, 1, "无法恢复频道").await;
                                }
                                
                                info!("✅ SendMessageHandler: 私聊频道恢复成功: {}", channel_id);
                                
                                // 重新获取频道并验证发送者在成员列表中
                                let channel = self.channel_service.get_channel(&channel_id).await
                                    .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))?;
                                
                                // ✨ 双重验证：确保发送者在成员列表中
                                if !channel.members.contains_key(&from_uid) {
                                    warn!("❌ SendMessageHandler: 恢复私聊频道后，发送者 {} 不在成员列表中", 
                                          send_message_request.from_uid);
                                    return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
                                }
                                
                                channel
                            } else {
                                warn!("❌ SendMessageHandler: 私聊会话缺少用户ID: {}", channel_id);
                                return self.create_error_response(&send_message_request, 1, "会话缺少用户ID").await;
                            }
                        }
                        crate::model::channel::ChannelType::Group => {
                            // 群聊：从数据库查询参与者并恢复频道
                            info!("🔧 SendMessageHandler: 恢复群聊频道: {}", channel_id);
                            
                            // 从数据库查询参与者
                            let participants = match self.channel_service.get_channel_participants(channel_id).await {
                                Ok(participants) => participants,
                        Err(e) => {
                                    warn!("❌ SendMessageHandler: 查询群聊参与者失败: {}", e);
                                    return self.create_error_response(&send_message_request, 1, "无法查询群聊参与者").await;
                                }
                            };
                            
                            if participants.is_empty() {
                                warn!("❌ SendMessageHandler: 群聊 {} 没有参与者", channel_id);
                                return self.create_error_response(&send_message_request, 1, "群聊没有参与者").await;
                            }
                            
                            // 获取群组名称（从会话元数据或使用默认值）
                            let group_name = channel.metadata.name.clone()
                                .unwrap_or_else(|| format!("群聊 {}", channel_id));
                            
                            // 找到群主（Owner 角色）或第一个参与者作为创建者
                            let owner_id = participants.iter()
                                .find(|p| p.role == crate::model::channel::MemberRole::Owner)
                                .map(|p| p.user_id)
                                .or_else(|| participants.first().map(|p| p.user_id))
                                .ok_or_else(|| {
                                    warn!("❌ SendMessageHandler: 无法确定群主");
                                    ServerError::Internal("无法确定群主".to_string())
                                })?;
                            
                            // 创建群聊频道
                            if let Err(e) = self.channel_service.create_group_chat_with_id(
                                owner_id,
                                group_name,
                                channel_id,
                            ).await {
                                warn!("❌ SendMessageHandler: 恢复群聊频道失败: {}", e);
                                return self.create_error_response(&send_message_request, 1, "无法恢复群聊频道").await;
                            }
                            
                            // 添加所有参与者到频道
                            for participant in &participants {
                                let user_id = participant.user_id;
                                let channel_role = match participant.role {
                                    crate::model::channel::MemberRole::Owner => crate::model::channel::MemberRole::Owner,
                                    crate::model::channel::MemberRole::Admin => crate::model::channel::MemberRole::Admin,
                                    crate::model::channel::MemberRole::Member => crate::model::channel::MemberRole::Member,
                                };
                                
                                // 跳过创建者（已在 create_group_chat_with_id 中添加）
                                if user_id == owner_id {
                                    continue;
                                }
                                
                                // 添加成员到频道
                                if let Err(e) = self.channel_service.add_member_to_group(
                                    channel_id,
                                    user_id,
                                ).await {
                                    warn!("⚠️ SendMessageHandler: 添加成员 {} 到群聊频道失败: {}", user_id, e);
                                    // 继续添加其他成员，不中断流程
                                } else {
                                    // 设置成员角色
                                    if let Err(e) = self.channel_service.set_member_role(
                                        &channel_id,
                                        &user_id,
                                        channel_role,
                                    ).await {
                                        warn!("⚠️ SendMessageHandler: 设置成员 {} 角色失败: {}", user_id, e);
                                    }
                                }
                            }
                            
                            info!("✅ SendMessageHandler: 群聊频道恢复成功: {} ({} 个成员)", 
                                  channel_id, participants.len());
                            
                            // 重新获取频道
                            let channel = self.channel_service.get_channel(&channel_id).await
                                .map_err(|e| ServerError::Internal(format!("获取恢复的频道失败: {}", e)))?;
                            
                            // ✨ 验证发送者是否在成员列表中
                            if !channel.members.contains_key(&from_uid) {
                                // 发送者不在成员列表中，尝试从参与者列表中添加
                                if let Some(participant) = participants.iter()
                                    .find(|p| p.user_id == send_message_request.from_uid) {
                                    let channel_role = match participant.role {
                                        crate::model::channel::MemberRole::Owner => crate::model::channel::MemberRole::Owner,
                                        crate::model::channel::MemberRole::Admin => crate::model::channel::MemberRole::Admin,
                                        crate::model::channel::MemberRole::Member => crate::model::channel::MemberRole::Member,
                                    };
                                    
                                    info!("🔧 SendMessageHandler: 发送者 {} 不在恢复的群聊频道成员列表中，从数据库参与者信息中添加", 
                                          send_message_request.from_uid);
                                    
                                    if let Err(e) = self.channel_service.join_channel(
                                        send_message_request.channel_id,
                                        send_message_request.from_uid.clone(),
                                        Some(channel_role),
                                    ).await {
                                        warn!("❌ SendMessageHandler: 添加发送者到恢复的群聊频道失败: {}", e);
                                        return self.create_error_response(&send_message_request, 4, "无法加入频道").await;
                                    }
                                    
                                    // 重新获取频道
                                    self.channel_service.get_channel(&send_message_request.channel_id).await
                                        .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))?
                                } else {
                                    warn!("❌ SendMessageHandler: 发送者 {} 不是群聊 {} 的参与者", 
                                          send_message_request.from_uid, send_message_request.channel_id);
                                    return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
                                }
                            } else {
                                channel
                            }
                        }
                        crate::model::channel::ChannelType::System => {
                            // 系统会话：不支持消息发送
                            warn!("❌ SendMessageHandler: 系统会话不支持消息发送: {}", send_message_request.channel_id);
                            return self.create_error_response(&send_message_request, 1, "系统会话不支持消息发送").await;
                        }
                    }
                } else {
                    // ✨ 频道和会话都不存在
                    // channel_id 现在是 u64，直接返回错误
                    warn!("❌ SendMessageHandler: 频道 {} 不存在且会话也不存在", send_message_request.channel_id);
                    return self.create_error_response(&send_message_request, 1, "频道不存在").await;
                }
            }
        };

        // 3. ✨ 确保发送者在频道成员列表中（如果不在，从数据库恢复）
        if !channel.members.contains_key(&from_uid) {
            // 用户不在内存频道中，尝试从数据库恢复
            if let Ok(db_channel) = self.channel_service.get_channel(&send_message_request.channel_id).await {
                // 检查用户是否是会话参与者
                let is_participant = match db_channel.channel_type {
                    crate::model::channel::ChannelType::Group => {
                        // 群聊：从数据库查询参与者
                        if let Ok(participants) = self.channel_service.get_channel_participants(send_message_request.channel_id).await {
                            participants.iter().any(|p| p.user_id == send_message_request.from_uid)
                        } else {
                            false
                        }
                    }
                    crate::model::channel::ChannelType::Direct => {
                        // 私聊：检查 direct_user1_id 和 direct_user2_id
                        db_channel.direct_user1_id.map(|id| id == from_uid).unwrap_or(false)
                            || db_channel.direct_user2_id.map(|id| id == from_uid).unwrap_or(false)
                    }
                    _ => false,
                };
                
                if is_participant {
                    // 用户是参与者，添加到频道
                    let role = match db_channel.channel_type {
                        crate::model::channel::ChannelType::Group => {
                            // 从数据库查询用户的角色
                            if let Ok(participants) = self.channel_service.get_channel_participants(channel_id).await {
                                participants.iter()
                                    .find(|p| p.user_id == from_uid)
                                    .map(|p| match p.role {
                                        crate::model::channel::MemberRole::Owner => crate::model::channel::MemberRole::Owner,
                                        crate::model::channel::MemberRole::Admin => crate::model::channel::MemberRole::Admin,
                                        crate::model::channel::MemberRole::Member => crate::model::channel::MemberRole::Member,
                                    })
                                    .unwrap_or(crate::model::channel::MemberRole::Member)
                            } else {
                                crate::model::channel::MemberRole::Member
                            }
                        }
                        _ => crate::model::channel::MemberRole::Member,
                    };
                    
                    info!("🔧 SendMessageHandler: 用户 {} 不在频道 {} 的成员列表中，但从数据库会话中发现是参与者，自动添加", 
                          from_uid, channel_id);
                    
                    if let Err(e) = self.channel_service.join_channel(
                        channel_id, 
                        from_uid, 
                        Some(role)
                    ).await {
                        warn!("❌ SendMessageHandler: 添加用户到频道失败: {}", e);
                        return self.create_error_response(&send_message_request, 4, "无法加入频道").await;
                    }
                    
                    // 重新获取频道（包含新成员）
                    channel = self.channel_service.get_channel(&send_message_request.channel_id).await
                        .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))?;
                } else {
                    // 用户不是参与者，拒绝访问
                    warn!("❌ SendMessageHandler: 用户 {} 不是频道 {} 的参与者", 
                          send_message_request.from_uid, send_message_request.channel_id);
                    return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
                }
            } else {
                // 会话不存在，拒绝访问
                warn!("❌ SendMessageHandler: 用户 {} 不在频道 {} 的成员列表中，且会话不存在", 
                      send_message_request.from_uid, send_message_request.channel_id);
                return self.create_error_response(&send_message_request, 4, "无权限访问此频道").await;
            }
        }
        
        // 3. ✅ 群组权限检查（禁言、全员禁言、发送权限）- 在 can_user_post 之前检查，以便返回正确的错误码
        if channel.channel_type == crate::model::channel::ChannelType::Group {
            // 获取成员信息
            if let Some(member) = channel.members.get(&from_uid) {
                // 3.1.1. 检查个人禁言状态（优先检查，返回错误码5）
                if member.is_muted {
                    // ChannelMember 只有 is_muted 字段，没有 mute_until
                    // 如果需要支持临时禁言，需要从 ChannelParticipant 查询 mute_until
                    warn!("❌ SendMessageHandler: 用户 {} 在群 {} 中被禁言", 
                          send_message_request.from_uid, send_message_request.channel_id);
                    return self.create_error_response(&send_message_request, 5, "您已被禁言").await;
                }
                
                // 3.1.2. 检查全员禁言（群主和管理员不受影响）
                if channel.settings.as_ref().map(|s| s.is_muted).unwrap_or(false) && 
                   !matches!(member.role, 
                       crate::model::channel::MemberRole::Owner | 
                       crate::model::channel::MemberRole::Admin
                   ) {
                    warn!("❌ SendMessageHandler: 群 {} 全员禁言中，用户 {} 无权发言", 
                          send_message_request.channel_id, send_message_request.from_uid);
                    return self.create_error_response(&send_message_request, 5, "群组全员禁言中").await;
                }
                
                // 3.1.3. 检查发送消息权限（基于角色的细粒度权限）
                let channel_role = match member.role {
                    crate::model::channel::MemberRole::Owner => crate::model::channel::MemberRole::Owner,
                    crate::model::channel::MemberRole::Admin => crate::model::channel::MemberRole::Admin,
                    crate::model::channel::MemberRole::Member => crate::model::channel::MemberRole::Member,
                };
                let permissions = crate::model::channel::MemberPermissions::from_role(channel_role);
                if !permissions.can_send_message {
                    warn!("❌ SendMessageHandler: 用户 {} 在群 {} 中没有发送消息权限", 
                          send_message_request.from_uid, send_message_request.channel_id);
                    return self.create_error_response(&send_message_request, 4, "您没有发送消息权限").await;
                }
            }
        }
        
        // 3.2. 检查用户是否可以发送消息（基础权限检查）- 在禁言检查之后
        if !channel.can_user_post(&from_uid) {
            // 如果用户不在成员列表中，错误已经在前面处理了
            // 这里主要是检查频道设置（如 allow_member_post）
            warn!("❌ SendMessageHandler: 用户 {} 无权限在频道 {} 发送消息（频道设置限制）", 
                  send_message_request.from_uid, send_message_request.channel_id);
            return self.create_error_response(&send_message_request, 4, "无权限发送消息").await;
        }

        // 4. 消息类型来自协议层 message_type（u32），payload 仅解析 content + metadata + reply_to_message_id + mentioned_user_ids + message_source
        let content_message_type = privchat_protocol::ContentMessageType::from_u32(send_message_request.message_type)
            .ok_or_else(|| ServerError::Validation("无效的 message_type".to_string()))?;
        let (content, metadata, reply_to_message_id, mentioned_user_ids, message_source) = match Self::parse_payload(&send_message_request.payload) {
            Ok(t) => t,
            Err(e) => {
                warn!("⚠️ SendMessageHandler: 解析 payload 失败，使用原始字符串: {}", e);
                (String::from_utf8_lossy(&send_message_request.payload).to_string(), None, None, Vec::new(), None)
            }
        };

        info!("📩 SendMessageHandler: message_type={}, content={}, metadata={:?}, reply_to={:?}, mentioned={:?}, source={:?}",
              send_message_request.message_type, content, metadata, reply_to_message_id, mentioned_user_ids, message_source);

        // 3.5. ✅ 检查黑名单和非好友消息权限（仅限私聊）
        if channel.channel_type == crate::model::channel::ChannelType::Direct {
            // 获取频道的所有成员（私聊应该只有2个成员）
            let members: Vec<u64> = channel.get_member_ids();
            
            // 找出接收者（不是发送者的那个用户）
            let receiver_id = members.iter()
                .find(|&id| *id != from_uid)
                .copied();
            
            if let Some(receiver_id) = receiver_id {
                // 3.5.1. 检查双向拉黑关系
                let (sender_blocks_receiver, receiver_blocks_sender) = self.blacklist_service
                    .check_mutual_block(from_uid, receiver_id)
                    .await
                    .unwrap_or((false, false));
                
                if receiver_blocks_sender {
                    // 接收者拉黑了发送者
                    warn!("🚫 SendMessageHandler: 用户 {} 已被 {} 拉黑，无法发送消息", 
                          from_uid, receiver_id);
                    return self.create_error_response(&send_message_request, 4, "您已被对方拉黑，无法发送消息").await;
                }
                
                if sender_blocks_receiver {
                    // 发送者拉黑了接收者（提示：防止误操作）
                    warn!("🚫 SendMessageHandler: 用户 {} 已拉黑 {}，无法发送消息", 
                          from_uid, receiver_id);
                    return self.create_error_response(&send_message_request, 4, "您已拉黑该用户，无法发送消息").await;
                }
                
                // 3.5.2. ✨ 检查非好友消息权限（类似QQ/Telegram/Zalo，用于客服系统）
                let are_friends = self.friend_service.is_friend(from_uid, receiver_id).await;
                
                if !are_friends {
                    // 不是好友，检查接收者的隐私设置
                    match self.privacy_service.get_or_create_privacy_settings(receiver_id).await {
                        Ok(privacy_settings) => {
                            if !privacy_settings.allow_receive_message_from_non_friend {
                                warn!("🚫 SendMessageHandler: 用户 {} 不允许接收非好友消息，发送者 {} 不是好友", 
                                      receiver_id, send_message_request.from_uid);
                                return self.create_error_response(&send_message_request, 4, "对方设置了仅接收好友消息，无法发送").await;
                            } else {
                                info!("✅ SendMessageHandler: 用户 {} 允许接收非好友消息，发送者 {} 可以发送", 
                                      receiver_id, send_message_request.from_uid);
                                
                                // ✨ 记录非好友消息来源（类似添加好友）
                                if let Some(ref source) = message_source {
                                    info!("📝 SendMessageHandler: 记录非好友消息来源: {} -> {} (source: {:?})", 
                                          send_message_request.from_uid, receiver_id, source);
                                    // TODO: 可以在这里记录到数据库或缓存，用于统计和分析
                                    // 例如：记录"用户A通过群组X向用户B发送了非好友消息"
                                }
                            }
                        }
                        Err(e) => {
                            warn!("⚠️ SendMessageHandler: 获取用户 {} 隐私设置失败: {}，默认允许非好友消息", receiver_id, e);
                            // 默认允许，继续发送
                        }
                    }
                } else {
                    debug!("✅ SendMessageHandler: 用户 {} 和 {} 是好友，允许发送消息", 
                           send_message_request.from_uid, receiver_id);
                }
            }
        }

        // 4.5. ✨ 验证引用消息（如果存在）
        let reply_to_message_preview = if let Some(ref reply_to_id) = reply_to_message_id {
            // 尝试解析 reply_to_id 是否为数字（seq）
            let replied_msg = if let Ok(seq) = reply_to_id.parse::<u64>() {
                // 如果是数字，通过 seq 查找
                match self.message_history_service.get_message_by_seq(&send_message_request.channel_id, seq).await {
                    Ok(msg) => msg,
                    Err(e) => {
                        warn!("❌ SendMessageHandler: 引用的消息 (seq={}) 不存在: {}", seq, e);
                        return self.create_error_response(&send_message_request, 3, "引用的消息不存在").await;
                    }
                }
            } else {
                // 如果不是数字，尝试解析为 u64 作为 message_id 查找
                match reply_to_id.parse::<u64>() {
                    Ok(msg_id) => {
                        match self.message_history_service.get_message(&msg_id).await {
                            Ok(msg) => msg,
                            Err(e) => {
                                warn!("❌ SendMessageHandler: 引用的消息 {} 不存在: {}", reply_to_id, e);
                                return self.create_error_response(&send_message_request, 3, "引用的消息不存在").await;
                            }
                        }
                    }
                    Err(_) => {
                        warn!("❌ SendMessageHandler: 无效的 reply_to_message_id 格式: {}", reply_to_id);
                        return self.create_error_response(&send_message_request, 3, "无效的引用消息ID格式").await;
                    }
                }
            };
            
            // 验证引用的消息是否在同一个频道
            if replied_msg.channel_id != send_message_request.channel_id {
                warn!("❌ SendMessageHandler: 引用的消息 {} 不在频道 {} 中", reply_to_id, send_message_request.channel_id);
                return self.create_error_response(&send_message_request, 3, "引用的消息不在当前频道中").await;
            }
            
            // 创建引用消息预览（最多50字符）
            let preview_content = replied_msg.content.chars().take(50).collect::<String>();
            Some(crate::service::ReplyMessagePreview {
                message_id: replied_msg.message_id.clone(),
                sender_id: replied_msg.sender_id.clone(),
                content: preview_content,
                message_type: replied_msg.message_type,
            })
        } else {
            None
        };

        // 4.6. 验证消息类型和 metadata（特别是 file_id）
        if let Err(e) = self.validate_message_metadata(
            content_message_type,
            &metadata,
            from_uid
        ).await {
            warn!("❌ SendMessageHandler: 消息验证失败: {}", e);
            return self.create_error_response(&send_message_request, 3, &format!("消息验证失败: {}", e)).await;
        }

        // 5. ✨ 处理@提及（使用客户端传递的用户ID列表，类似 Telegram）
        // 客户端在输入 @ 时已经选择了用户，所以直接使用 mentioned_user_ids
        // 消息内容中仍然显示 @用户昵称，但实际关联的是用户ID
        let is_mention_all = content.contains("@全体成员") || content.contains("@all") || content.contains("@everyone");

        // 5.1. 存储消息到历史记录（内存）
        let message_record = match self.message_history_service.store_message(
            &send_message_request.channel_id,
            &send_message_request.from_uid,
            content.clone(),
            send_message_request.message_type,
            reply_to_message_id.as_ref().and_then(|s| s.parse::<u64>().ok()),
            metadata.clone(),
        ).await {
            Ok(record) => record,
            Err(e) => {
                error!("❌ SendMessageHandler: 存储消息到内存失败: {}", e);
                return self.create_error_response(&send_message_request, 2, "存储消息失败").await;
            }
        };

        // ✨ 5.1.5. 保存消息到数据库
        // channel_id 现在直接是 u64，使用 channel.id 作为 channel_id
        let channel_id = channel.id;
        
        // 首先确保会话存在
        self.ensure_channel_and_members(channel_id, &channel).await;
        
        // 验证会话是否存在（通过数据库）
        // ✨ 如果会话仍然不存在，可能是异步创建延迟，等待一小段时间后重试
        let mut retry_count = 0;
        while self.channel_service.get_channel(&channel_id).await.is_err() && retry_count < 3 {
            retry_count += 1;
            warn!("⚠️ SendMessageHandler: 会话 {} 不存在，重试 {} (可能是异步创建延迟)", channel_id, retry_count);
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            // 再次尝试确保会话存在
            self.ensure_channel_and_members(channel_id, &channel).await;
        }
        
        // 最终验证会话是否存在
        if self.channel_service.get_channel(&channel_id).await.is_err() {
            error!("❌ SendMessageHandler: 会话 {} 不存在，无法保存消息", channel_id);
            return self.create_error_response(&send_message_request, 2, "会话不存在").await;
        }
        
        // 生成 pts（per-channel，Phase 8）
        let pts = self.pts_generator.next_pts(send_message_request.channel_id, send_message_request.channel_type).await;
        
        // 创建 Message 模型
        use crate::model::message::Message;
        let metadata_value = if let Some(ref meta_str) = metadata {
            serde_json::from_str(meta_str).unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()))
        } else {
            serde_json::Value::Object(serde_json::Map::new())
        };
        
        // ✨ 修复：使用内存中已生成的 message_id，确保客户端收到的 ID 和数据库中的一致
        let message_id = message_record.message_id;
        
        // 解析 reply_to_message_id 为 u64（如果存在）
        let reply_to_id = reply_to_message_id.and_then(|id| id.parse::<u64>().ok());
        
        let message = Message {
            message_id,
            channel_id,
            sender_id: from_uid,
            pts: Some(pts as i64),
            local_message_id: Some(send_message_request.local_message_id),
            content: content.clone(),
            message_type: content_message_type,
            metadata: metadata_value,
            reply_to_message_id: reply_to_id,
            created_at: message_record.created_at,
            updated_at: message_record.updated_at,
            deleted: false,
            deleted_at: None,
            revoked: false,
            revoked_at: None,
            revoked_by: None,
        };
        
        // 保存到数据库（必须成功，否则返回错误）
        if let Err(e) = self.message_repository.create(&message).await {
            error!("❌ SendMessageHandler: 保存消息到数据库失败: {}", e);
            return self.create_error_response(&send_message_request, 2, &format!("保存消息到数据库失败: {}", e)).await;
        }
        
        info!("✅ SendMessageHandler: 消息已保存到数据库: {}", message.message_id);
        
        // ✨ 发布 MessageCommitted 事件（用于 Push）
        // MVP 阶段：使用全局 EventBus（临时方案）
        if let Some(event_bus) = get_global_event_bus() {
            // 获取接收者 ID（私聊：另一个用户；群聊：所有其他成员）
            let recipient_ids: Vec<u64> = if channel.channel_type == crate::model::channel::ChannelType::Direct {
                // 私聊：获取另一个用户
                channel.get_member_ids()
                    .into_iter()
                    .filter(|&id| id != from_uid)
                    .collect()
            } else {
                // 群聊：所有其他成员
                channel.get_member_ids()
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
                    warn!("⚠️ SendMessageHandler: 发布 MessageCommitted 事件失败: {}", e);
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
                    crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
                );
                
                if !can_mention_all {
                    warn!("❌ SendMessageHandler: 用户 {} 无权@全体成员", send_message_request.from_uid);
                    return self.create_error_response(&send_message_request, 4, "无权@全体成员，仅群主和管理员可以").await;
                }
                
                // @全体成员时，获取所有成员ID
                let all_member_ids: Vec<u64> = channel.get_member_ids();
                // 记录@提及（包含所有成员）
                if let Err(e) = self.mention_service.record_mention(
                    message_record.message_id,
                    send_message_request.channel_id,
                    all_member_ids,
                    true,
                ).await {
                    warn!("⚠️ SendMessageHandler: 记录@全体成员失败: {}", e);
                }
            }
        } else if !mentioned_user_ids.is_empty() {
            // 记录@提及（使用客户端传递的用户ID列表）
            // 验证用户ID是否都在频道中
            let valid_user_ids: Vec<u64> = mentioned_user_ids.iter()
                .filter(|&&user_id| channel.members.contains_key(&user_id))
                .cloned()
                .collect();
            
            if valid_user_ids.len() != mentioned_user_ids.len() {
                warn!("⚠️ SendMessageHandler: 部分@的用户不在频道中，已过滤");
            }
            
            if !valid_user_ids.is_empty() {
                if let Err(e) = self.mention_service.record_mention(
                    message_record.message_id,
                    send_message_request.channel_id,
                    valid_user_ids,
                    false,
                ).await {
                    warn!("⚠️ SendMessageHandler: 记录@提及失败: {}", e);
                }
            }
        }

        // 5.1. 更新频道最后消息信息
        if let Err(e) = self.channel_service.update_last_message(channel_id, message_record.message_id).await {
            warn!("⚠️ SendMessageHandler: 更新频道最后消息信息失败: {}", e);
        }

        // 5.3. 确保会话存在，并将成员加入会话
        // channel_id 现在直接是 u64，使用 channel.id 作为 channel_id
        self.ensure_channel_and_members(channel.id, &channel).await;

        // 5.5. 更新会话的最后消息预览
        let last_message_preview = crate::service::LastMessagePreview {
            message_id: message_record.message_id,
            sender_id: from_uid,
            content: message_record.content.chars().take(50).collect(),
            message_type: content_message_type.as_str().to_string(),
            timestamp: message_record.created_at,
        };
        self.channel_service.update_last_message_preview(channel.id, last_message_preview).await;
        info!("✅ SendMessageHandler: 更新会话 {} 的最后消息预览", channel.id);

        // 6. 更新其他成员的未读计数
        if let Err(e) = self.update_member_unread_counts(&channel, &from_uid, &message_record.message_id).await {
            warn!("⚠️ SendMessageHandler: 更新未读计数失败: {}", e);
        }

        // 7. 分发消息到其他在线成员（传递引用消息预览和@提及信息）
        self.distribute_message_to_members(
            &channel, 
            &from_uid, 
            &message_record, 
            reply_to_message_preview.as_ref(),
            &mentioned_user_ids,  // ✨ 传递@提及的用户ID列表
            is_mention_all,        // ✨ 传递是否@全体成员
        ).await?;

        // 7.5. 标记消息为已处理（去重）
        self.message_dedup_service.mark_as_processed(from_uid, send_message_request.local_message_id).await;

        crate::infra::metrics::record_message_sent();

        // 8. 创建成功响应
        self.create_success_response(&send_message_request, &message_record).await
    }

    fn name(&self) -> &'static str {
        "SendMessageHandler"
    }
}

impl SendMessageHandler {
    /// 解析 payload 为 content + metadata + reply_to_message_id + mentioned_user_ids + message_source
    /// 消息类型由协议层 SendMessageRequest.message_type（u32）提供，不在此解析。
    /// Payload 格式见 protocol 层 MessagePayloadEnvelope。
    fn parse_payload(payload: &[u8]) -> crate::Result<(String, Option<String>, Option<String>, Vec<u64>, Option<crate::model::privacy::FriendRequestSource>)> {
        let json_value: Value = match serde_json::from_slice(payload) {
            Ok(v) => v,
            Err(_) => {
                return Ok((
                    String::from_utf8_lossy(payload).to_string(),
                    None,
                    None,
                    Vec::new(),
                    None
                ));
            }
        };

        // 提取 content
        let content = json_value
            .get("content")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| {
                // 如果没有 content 字段，尝试使用整个 JSON 作为字符串
                serde_json::to_string(&json_value).unwrap_or_else(|_| {
                    String::from_utf8_lossy(payload).to_string()
                })
            });

        // 提取 metadata
        let metadata = json_value
            .get("metadata")
            .map(|m| {
                serde_json::to_string(m).unwrap_or_else(|_| "{}".to_string())
            });

        // ✨ 提取 reply_to_message_id（可选）
        let reply_to_message_id = json_value
            .get("reply_to_message_id")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // ✨ 提取 mentioned_user_ids（可选）- 客户端已选择的用户ID列表
        let mentioned_user_ids: Vec<u64> = json_value
            .get("mentioned_user_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_u64())
                    .collect()
            })
            .unwrap_or_default();

        // ✨ 提取 message_source（可选）- 非好友消息来源
        let message_source: Option<crate::model::privacy::FriendRequestSource> = json_value
            .get("message_source")
            .and_then(|v| {
                let source_obj = v.as_object()?;
                let source_type = source_obj.get("type")?.as_str()?;
                let source_id = source_obj.get("source_id")?.as_str()?;
                
                match source_type {
                    "search" => {
                        source_id.parse::<u64>().ok().map(|search_session_id| {
                            crate::model::privacy::FriendRequestSource::Search { search_session_id }
                        })
                    },
                    "group" => {
                        source_id.parse::<u64>().ok().map(|group_id| {
                            crate::model::privacy::FriendRequestSource::Group { group_id }
                        })
                    },
                    "card_share" => {
                        source_id.parse::<u64>().ok().map(|share_id| {
                            crate::model::privacy::FriendRequestSource::CardShare { share_id }
                        })
                    },
                    "qrcode" => Some(crate::model::privacy::FriendRequestSource::Qrcode {
                        qrcode: source_id.to_string(),
                    }),
                    "phone" => Some(crate::model::privacy::FriendRequestSource::Phone {
                        phone: source_id.to_string(),
                    }),
                    _ => None,
                }
            });

        Ok((content, metadata, reply_to_message_id, mentioned_user_ids, message_source))
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
                if matches!(message_type, privchat_protocol::ContentMessageType::Text | privchat_protocol::ContentMessageType::System) {
                    return Ok(());
                }
                return Err(crate::error::ServerError::Validation(
                    format!("消息类型 {} 需要 metadata", message_type.as_str())
                ));
            }
        };
        
        let metadata: Value = serde_json::from_str(metadata_json)
            .map_err(|e| crate::error::ServerError::Validation(
                format!("metadata 不是有效的 JSON: {}", e)
            ))?;
        
        match message_type {
            privchat_protocol::ContentMessageType::Text | privchat_protocol::ContentMessageType::System => Ok(()),
            privchat_protocol::ContentMessageType::Image => self.validate_file_metadata(&metadata, "image", sender_id).await,
            privchat_protocol::ContentMessageType::Video => self.validate_file_metadata(&metadata, "video", sender_id).await,
            privchat_protocol::ContentMessageType::Voice => self.validate_file_metadata(&metadata, "voice", sender_id).await,
            privchat_protocol::ContentMessageType::Audio => self.validate_file_metadata(&metadata, "audio", sender_id).await,
            privchat_protocol::ContentMessageType::File => self.validate_file_metadata(&metadata, "file", sender_id).await,
            privchat_protocol::ContentMessageType::Location => self.validate_location_metadata(&metadata).await,
            privchat_protocol::ContentMessageType::ContactCard => self.validate_contact_card_metadata(&metadata).await,
            privchat_protocol::ContentMessageType::Sticker => self.validate_sticker_metadata(&metadata).await,
            privchat_protocol::ContentMessageType::Forward => self.validate_forward_metadata(&metadata).await,
        }
    }

    /// 验证文件类型消息的 metadata（image/video/audio/file）
    async fn validate_file_metadata(
        &self,
        metadata: &Value,
        file_type_name: &str,
        sender_id: u64,
    ) -> crate::Result<()> {
        // 验证 file_id 字段存在（扁平结构，支持数字或字符串）
        let file_id = metadata.get("file_id")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())))
            .ok_or_else(|| crate::error::ServerError::Validation(
                format!("{} 消息缺少或无效的 metadata.file_id", file_type_name)
            ))?;
        
        match self.file_service.get_file_metadata(file_id).await {
            Ok(Some(file_metadata)) => {
                // 验证文件所有权
                if file_metadata.uploader_id != sender_id {
                    return Err(crate::error::ServerError::Authorization(
                        format!("文件 {} 不属于发送者 {}", file_id, sender_id)
                    ));
                }
                info!("✅ 文件消息验证通过: type={}, file_id={}, uploader={}", 
                    file_type_name, file_id, file_metadata.uploader_id);
                Ok(())
            }
            Ok(None) => {
                Err(crate::error::ServerError::NotFound(
                    format!("文件 {} 不存在", file_id)
                ))
            }
            Err(e) => {
                warn!("⚠️ 验证文件 {} 时出错: {}", file_id, e);
                Err(crate::error::ServerError::Internal(e.to_string()))
            }
        }
    }

    /// 验证位置消息 metadata
    async fn validate_location_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证必需字段（扁平结构）
        metadata.get("latitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| crate::error::ServerError::Validation(
                "location 消息缺少有效的 latitude".to_string()
            ))?;
        
        metadata.get("longitude")
            .and_then(|v| v.as_f64())
            .ok_or_else(|| crate::error::ServerError::Validation(
                "location 消息缺少有效的 longitude".to_string()
            ))?;
        
        Ok(())
    }

    /// 验证名片消息 metadata
    async fn validate_contact_card_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证 user_id（扁平结构），接受 u64 或字符串类型
        metadata.get("user_id")
            .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse::<u64>().ok())))
            .ok_or_else(|| crate::error::ServerError::Validation(
                "contact_card 消息缺少 metadata.user_id (must be u64)".to_string()
            ))?;
        
        Ok(())
    }

    /// 验证表情包消息 metadata
    async fn validate_sticker_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证必需字段（扁平结构）
        metadata.get("sticker_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::ServerError::Validation(
                "sticker 消息缺少 metadata.sticker_id".to_string()
            ))?;
        
        metadata.get("image_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::error::ServerError::Validation(
                "sticker 消息缺少 metadata.image_url".to_string()
            ))?;
        
        Ok(())
    }

    /// 验证转发消息 metadata
    async fn validate_forward_metadata(&self, metadata: &Value) -> crate::Result<()> {
        // 验证 messages 数组（扁平结构）
        let messages = metadata.get("messages")
            .and_then(|v| v.as_array())
            .ok_or_else(|| crate::error::ServerError::Validation(
                "forward 消息缺少 metadata.messages 数组".to_string()
            ))?;
        
        if messages.is_empty() {
            return Err(crate::error::ServerError::Validation(
                "forward 消息的 messages 数组不能为空".to_string()
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
        self.channel_service.get_channel(&send_message_request.channel_id).await
            .map_err(|e| ServerError::Internal(format!("获取频道失败: {}", e)))
    }

    /// 更新其他成员的未读计数
    async fn update_member_unread_counts(
        &self,
        channel: &crate::model::channel::Channel,
        sender_id: &UserId,
        _message_id: &MessageId,
    ) -> Result<()> {
        // 获取频道所有成员
        let member_ids = channel.get_member_ids();
        
        // 为除发送者外的所有成员增加未读计数
        for member_id in member_ids {
            if member_id != *sender_id {
                // ✨ 使用新的 unread_count_service
                if let Err(e) = self.unread_count_service.increment(member_id, channel.id, 1).await {
                    warn!("⚠️ SendMessageHandler: 为用户 {} 增加未读计数失败: {}", member_id, e);
                } else {
                    debug!("📊 SendMessageHandler: 为用户 {} 在频道 {} 增加未读计数", 
                           member_id, channel.id);
                }
            }
        }
        
        Ok(())
    }

    /// 分发消息到其他在线成员
    async fn distribute_message_to_members(
        &self,
        channel: &crate::model::channel::Channel,
        sender_id: &UserId,
        message_record: &crate::service::message_history_service::MessageHistoryRecord,
        reply_to_message_preview: Option<&crate::service::ReplyMessagePreview>,
        mentioned_user_ids: &[u64],  // ✨ 添加@提及的用户ID列表
        is_mention_all: bool,           // ✨ 是否@全体成员
    ) -> Result<()> {
        info!("📡 SendMessageHandler: 分发消息 {} 到频道 {}", 
               message_record.message_id, channel.id);
        
        // 获取频道所有成员（包括发送者，用于消息回显）
        let all_member_ids: Vec<UserId> = channel.get_member_ids();
        
        if all_member_ids.is_empty() {
            debug!("📡 SendMessageHandler: 频道 {} 没有成员，无需分发", channel.id);
            return Ok(());
        }
        
        info!("📡 SendMessageHandler: 需要分发给 {} 个成员（包括发送者）: {:?}", 
              all_member_ids.len(), all_member_ids);
        
        // 创建 RecvRequest 消息（包含引用消息预览和@提及信息）
        let push_message_request = self.create_push_message_request(
            sender_id, 
            channel, 
            message_record, 
            reply_to_message_preview,
            mentioned_user_ids,  // ✨ 传递@提及的用户ID列表
            is_mention_all,       // ✨ 传递是否@全体成员
        ).await?;
        
        // 获取传输层服务器
        let transport_opt = self.transport.read().await.clone();
        if transport_opt.is_none() {
            warn!("⚠️ SendMessageHandler: TransportServer 未初始化，无法分发消息");
            return Ok(()); // 不返回错误，因为消息已存储
        }
        let transport = transport_opt.unwrap();
        
        // 为每个成员发送消息（包括发送者自己，实现消息回显）
        let mut success_count = 0;
        let mut failed_count = 0;
        
        for member_id in all_member_ids {
            let is_sender = &member_id == sender_id;
            
            // ✨ 为每个接收者生成 pts（per-channel，Phase 8）
            // 注意：Phase 8 中 pts 是 per-channel 的，不是 per-user
            // 所有用户共享同一个 channel 的 pts 序列
            if !is_sender {
                let pts = self.pts_generator.next_pts(channel.id, push_message_request.channel_type).await;
                debug!("✨ 为频道 {} 生成 pts={} (用户: {})", channel.id, pts, member_id);
                // 将 pts 和 message_id 存储到 UserMessageIndex
                let message_id = push_message_request.local_message_id;
                self.user_message_index.add_message(member_id, pts, message_id).await;
            }
            
            // 获取用户的会话信息
            if let Some(session_info) = self.session_manager.get_user_session_info(member_id).await {
                // 用户在线，发送消息
                // SessionId 是 u64 类型，需要从字符串解析
                // 如果 session_id 是 "session-1" 格式，提取数字部分
                let session_id_str = session_info.session_id.clone();
                let session_id = if session_id_str.starts_with("session-") {
                    // 提取 "session-1" 中的 "1"
                    if let Some(num_str) = session_id_str.strip_prefix("session-") {
                        num_str.parse::<u64>()
                            .map(SessionId::from)
                            .map_err(|e| ServerError::Internal(format!("解析会话ID失败: {} -> {}", session_id_str, e)))?
                    } else {
                        warn!("⚠️ SendMessageHandler: 无法解析会话ID格式: {}", session_id_str);
                        continue;
                    }
                } else {
                    // 直接解析为数字
                    session_id_str.parse::<u64>()
                        .map(SessionId::from)
                        .map_err(|e| ServerError::Internal(format!("解析会话ID失败: {} -> {}", session_id_str, e)))?
                };

                // READY 闸门：未 READY 不进行实时推送，交由离线补差处理
                if !self.auth_session_manager.is_ready_for_push(&session_id).await {
                    debug!(
                        "⏸️ SendMessageHandler: 用户 {} 会话 {} 未 READY，跳过实时推送并加入离线队列",
                        member_id, session_info.session_id
                    );
                    if let Err(queue_err) = self.offline_queue_service.add(member_id, &push_message_request).await {
                        error!("❌ SendMessageHandler: 添加离线消息失败: {}", queue_err);
                    } else if let Err(count_err) = self.unread_count_service.increment(member_id, channel.id, 1).await {
                        warn!("⚠️ SendMessageHandler: 更新未读计数失败: {}", count_err);
                    }
                    failed_count += 1;
                    continue;
                }
                
                // 编码 RecvRequest 消息
                let recv_data = privchat_protocol::encode_message(&push_message_request)
                    .map_err(|e| ServerError::Protocol(format!("编码 RecvRequest 失败: {}", e)))?;
                
                // 创建 Packet 并设置正确的 biz_type (RecvRequest = 7)
                // 使用一个临时的 message_id，TransportServer 可能会覆盖它
                let message_id = 0;
                let mut packet = Packet::one_way(message_id, recv_data);
                packet.set_biz_type(privchat_protocol::protocol::MessageType::PushMessageRequest as u8);
                
                // 对于发送者，使用异步非阻塞发送避免超时
                if is_sender {
                    let transport_clone = transport.clone();
                    let member_id_clone = member_id.clone();
                    let session_info_clone = session_info.clone();
                    tokio::spawn(async move {
                        match transport_clone.send_to_session(session_id, packet).await {
                            Ok(_) => {
                                info!("✅ SendMessageHandler: 成功发送消息回显到发送者 {} (会话: {})", 
                                      member_id_clone, session_info_clone.session_id);
                            }
                            Err(e) => {
                                debug!("⚠️ SendMessageHandler: 发送消息回显失败: {}（非关键错误）", e);
                            }
                        }
                    });
                    success_count += 1;
                } else {
                    // 对于其他成员，正常同步发送
                    match transport.send_to_session(session_id, packet).await {
                        Ok(_) => {
                            info!("✅ SendMessageHandler: 成功发送消息到用户 {} (会话: {})", 
                                  member_id, session_info.session_id);
                            success_count += 1;
                            
                            // ✨ Phase 3.5: 发布 MessageDelivered 事件（取消 Push Intent）
                            if let Some(event_bus) = get_global_event_bus() {
                                // 从 session_info 中获取 device_id（从 online_devices 中取第一个）
                                let device_id = session_info.online_devices.first()
                                    .map(|d| d.device_id.clone())
                                    .unwrap_or_else(|| format!("device_{}", session_info.session_id));
                                
                                let event = crate::domain::events::DomainEvent::MessageDelivered {
                                    message_id: push_message_request.server_message_id,
                                    user_id: member_id,
                                    device_id,
                                    timestamp: chrono::Utc::now().timestamp(),
                                };
                                
                                if let Err(e) = event_bus.publish(event) {
                                    warn!("⚠️ SendMessageHandler: 发布 MessageDelivered 事件失败: {}", e);
                                }
                            }
                        }
                        Err(e) => {
                            warn!("❌ SendMessageHandler: 发送消息到用户 {} 失败: {}，将加入离线队列", member_id, e);
                            failed_count += 1;
                            
                            // ✨ Phase 3.5: 检查是否需要创建 Push Intent
                            if let Some(user_device_repo) = &self.user_device_repo {
                                if let Ok(devices) = user_device_repo.get_user_devices(member_id).await {
                                    for device in devices {
                                        // 检查是否需要 Push：apns_armed = true 且 connected = false
                                        if device.apns_armed && !device.connected {
                                            // 发布 MessageCommitted 事件（触发设备级 Push Intent）
                                            if let Some(event_bus) = get_global_event_bus() {
                                                // 从 message_record 中提取内容预览
                                                let content_preview = message_record.content
                                                    .chars()
                                                    .take(50)
                                                    .collect::<String>();
                                                
                                                let event = crate::domain::events::DomainEvent::MessageCommitted {
                                                    message_id: push_message_request.server_message_id,
                                                    conversation_id: channel.id,
                                                    sender_id: *sender_id,
                                                    recipient_id: member_id,
                                                    content_preview,
                                                    message_type: message_record.message_type.to_string(),  // u32 -> String
                                                    timestamp: message_record.created_at.timestamp(),
                                                    device_id: Some(device.device_id),  // ✨ Phase 3.5: 设备级 Intent
                                                };
                                                
                                                if let Err(e) = event_bus.publish(event) {
                                                    warn!("⚠️ SendMessageHandler: 发布 MessageCommitted 事件失败: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            
                            // ✨ 使用新的离线队列服务
                            if let Err(queue_err) = self.offline_queue_service.add(member_id, &push_message_request).await {
                                error!("❌ SendMessageHandler: 添加离线消息失败: {}", queue_err);
                            } else {
                                info!("✅ SendMessageHandler: 已将消息加入用户 {} 的离线队列", member_id);
                                // ✨ 更新未读计数
                                if let Err(count_err) = self.unread_count_service.increment(member_id, channel.id, 1).await {
                                    warn!("⚠️ SendMessageHandler: 更新未读计数失败: {}", count_err);
                                }
                            }
                        }
                    }
                }
            } else {
                debug!("📡 SendMessageHandler: 用户 {} 不在线，将加入离线队列", member_id);
                
                // ✨ Phase 3.5: 检查是否需要创建 Push Intent
                if let Some(user_device_repo) = &self.user_device_repo {
                    if let Ok(devices) = user_device_repo.get_user_devices(member_id).await {
                        for device in devices {
                            // 检查是否需要 Push：apns_armed = true 且 connected = false
                            if device.apns_armed && !device.connected {
                                // 发布 MessageCommitted 事件（触发设备级 Push Intent）
                                if let Some(event_bus) = get_global_event_bus() {
                                    // 从 message_record 中提取内容预览
                                    let content_preview = message_record.content
                                        .chars()
                                        .take(50)
                                        .collect::<String>();
                                    
                                    let event = crate::domain::events::DomainEvent::MessageCommitted {
                                        message_id: push_message_request.server_message_id,
                                        conversation_id: channel.id,
                                        sender_id: *sender_id,
                                        recipient_id: member_id,
                                        content_preview,
                                        message_type: message_record.message_type.to_string(),  // u32 -> String
                                        timestamp: message_record.created_at.timestamp(),
                                        device_id: Some(device.device_id),  // ✨ Phase 3.5: 设备级 Intent
                                    };
                                    
                                    if let Err(e) = event_bus.publish(event) {
                                        warn!("⚠️ SendMessageHandler: 发布 MessageCommitted 事件失败: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                
                // ✨ 使用新的离线队列服务
                if let Err(queue_err) = self.offline_queue_service.add(member_id, &push_message_request).await {
                    error!("❌ SendMessageHandler: 添加离线消息失败: {}", queue_err);
                } else {
                    info!("✅ SendMessageHandler: 已将消息加入用户 {} 的离线队列", member_id);
                    // ✨ 更新未读计数
                    if let Err(count_err) = self.unread_count_service.increment(member_id, channel.id, 1).await {
                        warn!("⚠️ SendMessageHandler: 更新未读计数失败: {}", count_err);
                    }
                }
            }
        }
        
        info!("📡 SendMessageHandler: 消息分发完成 - 成功: {}, 失败: {}", success_count, failed_count);
        
        Ok(())
    }
    
    /// 创建 RecvRequest 消息
    async fn create_push_message_request(
        &self,
        sender_id: &UserId,
        channel: &crate::model::channel::Channel,
        message_record: &crate::service::message_history_service::MessageHistoryRecord,
        reply_to_message_preview: Option<&crate::service::ReplyMessagePreview>,
        mentioned_user_ids: &[u64],  // ✨ @提及的用户ID列表
        is_mention_all: bool,           // ✨ 是否@全体成员
    ) -> Result<privchat_protocol::protocol::PushMessageRequest> {
        use std::time::{SystemTime, UNIX_EPOCH};
        
        // 生成消息键
        let msg_key = format!("msg_{}", message_record.message_id);
        
        // 获取时间戳
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| ServerError::Internal(format!("获取时间戳失败: {}", e)))?
            .as_secs() as u32;
        
        // 构建 payload（包含 content + metadata + reply_to）
        let mut payload_json = serde_json::Map::new();
        payload_json.insert("content".to_string(), serde_json::Value::String(message_record.content.clone()));
        
        // 添加 metadata（如果存在）
        if let Some(ref metadata_str) = message_record.metadata {
            if let Ok(metadata_value) = serde_json::from_str::<Value>(metadata_str) {
                payload_json.insert("metadata".to_string(), metadata_value);
            }
        }
        
        // ✨ 添加引用消息预览（如果存在）
        if let Some(ref reply_preview) = reply_to_message_preview {
            payload_json.insert("reply_to".to_string(), serde_json::json!({
                "message_id": reply_preview.message_id,
                "sender_id": reply_preview.sender_id,
                "content": reply_preview.content,
                "message_type": reply_preview.message_type
            }));
        }
        
        // ✨ 添加@提及信息（如果存在）
        if !mentioned_user_ids.is_empty() || is_mention_all {
            payload_json.insert("mentioned_user_ids".to_string(), serde_json::json!(mentioned_user_ids));
            payload_json.insert("is_mention_all".to_string(), serde_json::json!(is_mention_all));
        }
        
        let payload_json_value = serde_json::Value::Object(payload_json);
        let payload = serde_json::to_string(&payload_json_value)
            .map_err(|e| ServerError::Internal(format!("序列化 payload 失败: {}", e)))?
            .into_bytes();

        // 创建 RecvRequest
        let push_message_request = privchat_protocol::protocol::PushMessageRequest {
            setting: privchat_protocol::protocol::MessageSetting::default(),
            msg_key: msg_key.clone(),
            server_message_id: message_record.message_id,
            message_seq: message_record.seq as u32,
            local_message_id: message_record.message_id,
            stream_no: String::new(),
            stream_seq: 0,
            stream_flag: 0,
            timestamp,
            channel_id: channel.id,
            channel_type: match channel.channel_type {
                crate::model::channel::ChannelType::Direct => 0u8,
                crate::model::channel::ChannelType::Group => 1u8,
                crate::model::channel::ChannelType::System => 2u8,
            },
            message_type: message_record.message_type,
            expire: 0,
            topic: String::new(),
            from_uid: *sender_id,
            payload,
        };
        
        Ok(push_message_request)
    }

    /// 创建成功响应
    async fn create_success_response(
        &self,
        request: &privchat_protocol::protocol::SendMessageRequest,
        message_record: &crate::service::message_history_service::MessageHistoryRecord,
    ) -> Result<Option<Vec<u8>>> {
        // message_id 现在已经是 u64 类型，直接使用
        let response = privchat_protocol::protocol::SendMessageResponse {
            client_seq: request.client_seq,
            server_message_id: message_record.message_id,
            message_seq: message_record.seq as u32,
            reason_code: 0, // 成功
        };
        
        let response_bytes = privchat_protocol::encode_message(&response)
            .map_err(|e| ServerError::Protocol(format!("编码响应失败: {}", e)))?;
        
        info!("✅ SendMessageHandler: 消息发送成功 - 消息ID: {}, 序号: {}", 
              message_record.message_id, message_record.seq);
        
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
        
        info!("🔄 SendMessageHandler: 重复消息已忽略 - 用户: {}, local_message_id: {}", 
              request.from_uid, request.local_message_id);
        
        Ok(Some(response_bytes))
    }

    /// 创建错误响应
    async fn create_error_response(
        &self, 
        request: &privchat_protocol::protocol::SendMessageRequest, 
        error_code: u8, 
        error_message: &str
    ) -> Result<Option<Vec<u8>>> {
        let response = privchat_protocol::protocol::SendMessageResponse {
            client_seq: request.client_seq,
            server_message_id: 0, // 错误时使用0
            message_seq: 0,
            reason_code: error_code,
        };
        
        let response_bytes = privchat_protocol::encode_message(&response)
            .map_err(|e| ServerError::Protocol(format!("编码错误响应失败: {}", e)))?;
        
        warn!("❌ SendMessageHandler: 消息发送失败 - 错误码: {}, 错误信息: {}", 
              error_code, error_message);
        
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
    async fn ensure_channel_and_members(&self, channel_id: u64, channel: &crate::model::channel::Channel) {
        use crate::model::channel::{ChannelType, CreateChannelRequest, MemberRole};
        
        // 获取频道成员列表
        let member_ids: Vec<u64> = channel.members.keys().cloned().collect();
        
        if member_ids.is_empty() {
            warn!("⚠️ SendMessageHandler: 频道 {} 没有成员，跳过会话创建", channel_id);
            return;
        }
        
        // 检查会话是否存在
        let channel_exists = self.channel_service.get_channel(&channel_id).await.is_ok();
        
        if !channel_exists {
            // 会话不存在，自动创建会话
            let channel_type = match channel.channel_type {
                crate::model::channel::ChannelType::Direct => ChannelType::Direct,
                crate::model::channel::ChannelType::Group => ChannelType::Group,
                crate::model::channel::ChannelType::System => ChannelType::System,
            };
            
            // 使用第一个成员作为创建者（通常是频道的owner）
            let creator_id = member_ids.first().copied().unwrap_or(0);
            
            let create_request = CreateChannelRequest {
                channel_type,
                name: None, // 会话名称从频道获取
                description: None,
                member_ids: member_ids.iter().skip(1).copied().collect(), // 跳过创建者
                is_public: Some(false),
                max_members: None,
            };
            
            // 使用固定的会话ID（与频道ID相同）
            match self.channel_service.create_channel_with_id(
                channel_id,
                creator_id,
                create_request
            ).await {
                Ok(_) => {
                    info!("✅ SendMessageHandler: 为频道 {} 自动创建了会话", channel_id);
                }
                Err(e) => {
                    warn!("⚠️ SendMessageHandler: 为频道 {} 创建会话失败: {}", channel_id, e);
                    return;
                }
            }
        }
        
        // 确保所有成员都在会话中
        for member_id in &member_ids {
            // 检查用户是否已在会话中
            let user_channels = self.channel_service.get_user_channels_enhanced(*member_id).await;
            let already_in_channel = user_channels.channels.iter()
                .any(|item| item.channel.id == channel_id);
            
            if !already_in_channel {
                // 将用户加入会话
                if let Err(e) = self.channel_service.join_channel(
                    channel_id,
                    member_id.clone(),
                    Some(MemberRole::Member), // 明确指定角色
                ).await {
                    warn!("⚠️ SendMessageHandler: 用户 {} 加入会话 {} 失败: {}", member_id, channel_id, e);
                } else {
                    info!("✅ SendMessageHandler: 用户 {} 已加入会话 {}", member_id, channel_id);
                }
            }
        }
    }
} 
