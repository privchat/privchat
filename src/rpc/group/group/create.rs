use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{RpcServiceContext, helpers};
use privchat_protocol::rpc::group::group::GroupCreateRequest;

/// 处理 创建群组 请求
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 创建群组 请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupCreateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 creator_id
    request.creator_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let name = &request.name;
    let description = request.description.as_deref().unwrap_or("");
    let creator_id = request.creator_id;
    
        // 检查创建者是否存在（从数据库读取）
        match helpers::get_user_profile_with_fallback(creator_id, &services.user_repository, &services.cache_manager).await {
            Ok(Some(_)) => {
                // ✨ 确保用户存在于数据库中（用于外键约束）
                // 检查用户是否存在于数据库中
                let user_exists = services.user_repository.exists(creator_id).await
                    .unwrap_or(false);
                
                if !user_exists {
                    tracing::warn!("⚠️ 用户 {} 不存在于数据库中，尝试创建", creator_id);
                    // 如果用户不存在，尝试从缓存获取信息并创建用户
                    if let Ok(Some(profile)) = services.cache_manager.get_user_profile(creator_id).await {
                        use crate::model::user::User;
                        let mut user = User::new(
                            creator_id,
                            profile.username.clone(),
                        );
                        user.phone = profile.phone.clone();
                        user.email = profile.email.clone();
                        user.display_name = Some(profile.nickname.clone());
                        user.avatar_url = profile.avatar_url.clone();
                        
                        if let Err(e) = services.user_repository.create(&user).await {
                            tracing::error!("❌ 创建用户失败: {}", e);
                            return Err(RpcError::internal(format!("无法创建用户: {}", e)));
                        }
                        tracing::info!("✅ 用户已创建: {}", creator_id);
                    } else {
                        return Err(RpcError::not_found(format!("用户 {} 不存在于数据库中，且无法从缓存获取", creator_id)));
                    }
                }
                
                // ✨ 创建数据库会话（channel_service 会在内部创建群组记录）
                use crate::model::channel::{CreateChannelRequest, ChannelType};
                
                let create_request = CreateChannelRequest {
                    channel_type: ChannelType::Group,
                    name: Some(name.to_string()),
                    description: if description.is_empty() { None } else { Some(description.to_string()) },
                    member_ids: vec![], // 初始成员只有创建者，已在 Channel::new_group 中添加
                    is_public: Some(false),
                    max_members: None,
                };
                
                // 创建数据库会话（让数据库自动生成 channel_id）
                match services.channel_service.create_channel(creator_id, create_request).await {
                    Ok(response) => {
                        // ✨ 检查响应是否成功
                        if !response.success {
                            let error_msg = response.error.unwrap_or_else(|| "创建会话失败".to_string());
                            tracing::error!("❌ 创建群聊会话失败: {}", error_msg);
                            return Err(RpcError::internal(format!("创建群聊会话失败: {}", error_msg)));
                        }
                        
                        let actual_channel_id = response.channel.id;
                        if actual_channel_id == 0 {
                            tracing::error!("❌ 创建群聊会话失败: channel_id 为 0");
                            return Err(RpcError::internal("创建群聊会话失败: channel_id 为 0".to_string()));
                        }
                        
                        tracing::info!("✅ 群聊会话已创建: {}", actual_channel_id);
                        
                        // 使用 channel_id 创建 Channel（确保 Channel 和 Channel 使用相同的 ID）
                        match services.channel_service.create_group_chat_with_id(
                            creator_id,
                            name.to_string(),
                            actual_channel_id,
                        ).await {
                            Ok(_) => {
                                tracing::info!("✅ 群组创建成功: {} -> {}", name, actual_channel_id);
                            }
                            Err(e) => {
                                tracing::warn!("⚠️ 创建群组频道失败: {}，频道可能已存在", e);
                            }
                        }
                        
                        // 返回客户端期望的群组信息格式（返回 channel_id，客户端应使用此 ID 发送消息）
                        Ok(json!({
                            "group_id": actual_channel_id, // ✨ 返回会话 ID 给客户端
                            "name": name,
                            "description": description,
                            "member_count": 1,
                            "created_at": chrono::Utc::now().to_rfc3339(),
                            "creator_id": creator_id
                        }))
                    }
                    Err(e) => {
                        tracing::error!("❌ 创建群聊会话失败: {}", e);
                        Err(RpcError::internal(format!("创建群聊会话失败: {}", e)))
                    }
                }
            }
            Ok(None) => Err(RpcError::not_found(format!("Creator '{}' not found", creator_id))),
            Err(e) => {
                tracing::error!("Failed to get user profile: {}", e);
                Err(RpcError::internal("Database error".to_string()))
            }
        }
}
