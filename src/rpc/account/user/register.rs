use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::auth::UserRegisterRequest;
use crate::auth::hash_password;
use crate::model::user::User;
use crate::repository::MessageRepository;
use uuid::Uuid;

/// 处理用户注册请求
/// 
/// 根据服务器配置 `use_internal_auth` 决定是否启用：
/// - true: 使用服务器内置的注册功能，返回 JWT token（适合独立部署）
/// - false: 返回错误，提示使用外部认证系统（适合企业集成）
/// 
/// 注册成功后返回 JWT token，客户端可直接用于 AuthorizationRequest
pub async fn handle(body: Value, services: RpcServiceContext, _ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理用户注册请求: {:?}", body);
    
    // 检查是否启用内置账号系统
    if !services.config.use_internal_auth {
        tracing::warn!("❌ 内置账号系统已禁用，请使用外部认证系统");
        return Err(RpcError::forbidden(
            "内置账号系统已禁用。请使用外部认证系统进行用户注册。".to_string()
        ));
    }
    
    // ✨ 使用协议层类型自动反序列化
    let request: UserRegisterRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 参数验证
    if request.username.is_empty() {
        return Err(RpcError::validation("用户名不能为空".to_string()));
    }
    
    if request.password.len() < 6 {
        return Err(RpcError::validation("密码至少需要 6 个字符".to_string()));
    }
    
    // 验证 device_id 必须是 UUID 格式（与认证时的验证保持一致）
    if Uuid::parse_str(&request.device_id).is_err() {
        return Err(RpcError::validation(
            "device_id 必须是有效的 UUID 格式".to_string()
        ));
    }
    
    // 1. 检查用户名是否已存在
    if let Ok(Some(_)) = services.user_repository.find_by_username(&request.username).await {
        return Err(RpcError::validation("用户名已存在".to_string()));
    }
    
    // 2. 加密密码
    let password_hash = hash_password(&request.password)
        .map_err(|e| RpcError::internal(format!("密码加密失败: {}", e)))?;
    
    // 3. 创建用户对象
    let mut user = User::new_with_password(0, request.username.clone(), password_hash);
    user.email = request.email.clone();
    user.display_name = request.nickname.clone();
    user.phone = request.phone.clone();
    
    // 4. 保存到数据库（数据库自动生成 user_id）
    let created_user = services.user_repository.create(&user).await
        .map_err(|e| match e {
            crate::error::DatabaseError::DuplicateEntry(msg) => RpcError::validation(msg),
            _ => RpcError::internal(format!("创建用户失败: {}", e))
        })?;
    
    let user_id = created_user.id;
    let device_id = &request.device_id;
    
    // 5. 从 device_info 中提取 app_id 和 device_type
    let (app_id, device_type_str) = request.device_info.as_ref()
        .map(|info| (info.app_id.clone(), info.device_type.as_str()))
        .unwrap_or_else(|| ("unknown".to_string(), "unknown"));
    
    // 6. 注册设备并获取 session_version ✨
    let device = crate::auth::Device {
        device_id: device_id.to_string(),
        user_id,
        business_system_id: "privchat-internal".to_string(),
        device_info: crate::auth::DeviceInfo {
            app_id: app_id.clone(),
            device_name: format!("{} Device", app_id),
            device_model: "Unknown".to_string(),
            os_version: "Unknown".to_string(),
            app_version: "1.0.0".to_string(),
        },
        device_type: match device_type_str.as_ref() {
            "ios" => crate::auth::DeviceType::IOS,
            "android" => crate::auth::DeviceType::Android,
            "web" => crate::auth::DeviceType::Web,
            "macos" => crate::auth::DeviceType::MacOS,
            "windows" => crate::auth::DeviceType::Windows,
            _ => crate::auth::DeviceType::Unknown,
        },
        token_jti: String::new(), // 稍后设置
        session_version: 1, // 新设备，版本号为 1
        session_state: crate::auth::SessionState::Active,
        kicked_at: None,
        kicked_reason: None,
        last_active_at: chrono::Utc::now(),
        created_at: chrono::Utc::now(),
        ip_address: "127.0.0.1".to_string(), // TODO: 从请求中获取真实IP
    };
    
    services.device_manager_db
        .register_or_update_device(&device)
        .await
        .map_err(|e| RpcError::internal(format!("注册设备失败: {}", e)))?;
    
    tracing::info!("✅ 设备已注册: device_id={}, session_version=1", device_id);
    
    // 7. 使用 JWT 服务生成真实的 JWT token（带 session_version）✨
    // Token payload 包含：
    // - sub: user_id（用户ID）
    // - device_id: 设备ID（用于验证）
    // - business_system_id: 业务系统ID（内置认证使用 "privchat-internal"）
    // - app_id: 应用ID（从 device_info 获取）
    // - session_version: 会话版本号（用于设备级撤销）✨ 新增
    // - jti: JWT ID（用于撤销）
    // - iss, aud, exp, iat: JWT 标准字段
    let token = services.jwt_service.issue_token_with_version(
        user_id,
        device_id,
        "privchat-internal", // 内置认证系统的业务系统ID
        &app_id,
        1, // ✨ 新设备使用版本号 1
        None, // 使用默认 TTL
    ).map_err(|e| RpcError::internal(format!("生成 JWT token 失败: {}", e)))?;
    
    // 8. 计算过期时间
    let token_ttl = services.jwt_service.default_ttl();
    let expires_at = (chrono::Utc::now() + chrono::Duration::seconds(token_ttl)).to_rfc3339();
    
    // TODO: 生成 refresh_token（目前暂时使用相同的 token）
    let refresh_token = token.clone();
    
    tracing::info!(
        "✅ 用户注册成功: username={}, user_id={}, device_id={}, device_type={}, app_id={}", 
        request.username, user_id, device_id, device_type_str, app_id
    );
    
    // 9. 系统消息功能：创建与系统用户的会话并发送欢迎消息
    if services.config.system_message.enabled && services.config.system_message.auto_create_channel {
        tracing::info!("🤖 为新用户创建系统消息会话: user_id={}", user_id);
        
        // 创建与系统用户的私聊会话
        let create_request = crate::model::CreateChannelRequest {
            channel_type: crate::model::ChannelType::Direct,
            name: None,
            description: None,
            member_ids: vec![crate::config::SYSTEM_USER_ID],
            is_public: Some(false),
            max_members: None,
        };
        
        match services.channel_service.create_channel(user_id, create_request).await {
            Ok(response) if response.success => {
                let channel_id = response.channel.id;
                tracing::info!("✅ 系统消息会话创建成功: channel_id={}", channel_id);
                
                // 如果配置了自动发送欢迎消息，插入欢迎消息到数据库，并加入离线推送流程
                if services.config.system_message.auto_send_welcome {
                    let now = chrono::Utc::now();
                    let message_id = crate::infra::next_message_id();
                    // pts 为 per-channel，Direct 类型=0
                    let pts = services.pts_generator
                        .next_pts(channel_id, crate::model::channel::ChannelType::Direct as u8)
                        .await;
                    let content = services.config.system_message.welcome_message.clone();

                    let welcome_msg = crate::model::message::Message {
                        message_id,
                        channel_id,
                        sender_id: crate::config::SYSTEM_USER_ID,
                        pts: Some(pts as i64),
                        local_message_id: Some(message_id),
                        content: content.clone(),
                        message_type: crate::model::message::MessageType::Text,
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

                    match services.message_repository.create(&welcome_msg).await {
                        Ok(_) => {
                            // ✨ 加入 UserMessageIndex，否则离线推送时 get_message_ids_above 查不到
                            services.user_message_index
                                .add_message(user_id, pts, message_id)
                                .await;

                            // ✨ 加入 OfflineQueueService，否则离线推送时 get_all 取不到消息
                            let payload_json = json!({ "content": content });
                            let payload = serde_json::to_vec(&payload_json)
                                .unwrap_or_else(|_| Vec::new());

                            let push_msg = privchat_protocol::message::PushMessageRequest {
                                setting: privchat_protocol::message::MessageSetting::default(),
                                msg_key: format!("msg_{}", message_id),
                                server_message_id: message_id,
                                message_seq: 1,
                                local_message_id: message_id,
                                stream_no: String::new(),
                                stream_seq: 0,
                                stream_flag: 0,
                                timestamp: now.timestamp().max(0) as u32,
                                channel_id,
                                channel_type: 0, // Direct
                                expire: 0,
                                topic: String::new(),
                                from_uid: crate::config::SYSTEM_USER_ID,
                                payload,
                            };

                            if let Err(e) = services.offline_queue_service.add(user_id, &push_msg).await {
                                tracing::warn!("⚠️ 欢迎消息加入离线队列失败: {:?}", e);
                            }

                            tracing::info!(
                                "✅ 系统欢迎消息已发送: channel_id={}, message_id={}, user_id={}",
                                channel_id, message_id, user_id
                            );
                        }
                        Err(e) => {
                            tracing::warn!("⚠️ 发送系统欢迎消息失败: {:?}", e);
                        }
                    }
                }
            }
            Ok(response) => {
                tracing::warn!("⚠️ 创建系统消息会话失败: {:?}", response.error);
            }
            Err(e) => {
                tracing::warn!("⚠️ 创建系统消息会话异常: {:?}", e);
            }
        }
    }
    
    // 10. 返回统一的 AuthResponse 格式（包含 device_id）
    Ok(json!({
        "success": true,
        "user_id": user_id,
        "token": token,
        "refresh_token": refresh_token,
        "expires_at": expires_at,
        "device_id": device_id,
        "message": "注册成功"
    }))
} 