use serde_json::Value;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::model::privacy::{SearchType, UserPrivacySettings};
use privchat_protocol::rpc::account::search::{AccountSearchQueryRequest, AccountSearchResponse, SearchedUser};

/// 处理 用户搜索 请求（精确搜索）
/// 
/// 通过关键词精确搜索用户，支持搜索 username、phone、email
/// 输入必须完全匹配，例如输入 "alice_xxx" 只会返回该用户
/// 
/// 返回结果包含 search_session_id，用于后续查看用户详情
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理用户搜索请求: {:?}", body);
    
    // ✨ 使用协议层类型自动反序列化
    let mut request: AccountSearchQueryRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;
    
    // 从 ctx 填充 from_user_id
    request.from_user_id = crate::rpc::get_current_user_id(&ctx)?;
    
    let searcher_id = request.from_user_id;
    let query = &request.query;
    
    if query.is_empty() {
        return Err(RpcError::validation("query cannot be empty".to_string()));
    }
    
    let mut results = Vec::new();
    
    // ✨ 关键字精确搜索：按用户名查找
    let user_opt = services.user_repository.find_by_username(query).await.ok().flatten();
    
    // 处理找到的用户
    if let Some(user) = user_opt {
        let user_id = user.id;
        
        // 1. 检查隐私设置：是否允许被搜索（用户名搜索）
        let privacy = services.privacy_service.get_or_create_privacy_settings(user_id).await
            .unwrap_or_else(|_| UserPrivacySettings::new(user_id));
        
        if privacy.allows_search(SearchType::Username) {
            // 2. 检查好友关系和发消息权限
            let is_friend = services.friend_service.is_friend(searcher_id, user_id).await;
            
            // 检查是否可以发消息
            let can_send_message = {
                // 如果是好友，可以发消息
                if is_friend {
                    true
                } else {
                    // 不是好友，检查黑名单和隐私设置
                    let (sender_blocks_receiver, receiver_blocks_sender) = services.blacklist_service
                        .check_mutual_block(searcher_id, user_id)
                        .await
                        .unwrap_or((false, false));
                    
                    // 如果被拉黑，不能发消息
                    if receiver_blocks_sender || sender_blocks_receiver {
                        false
                    } else {
                        // 检查隐私设置：是否允许接收非好友消息
                        match services.privacy_service.get_or_create_privacy_settings(user_id).await {
                            Ok(privacy_settings) => privacy_settings.allow_receive_message_from_non_friend,
                            Err(_) => true, // 默认允许
                        }
                    }
                }
            };
            
            // 3. 创建搜索记录
            match services.cache_manager.create_search_record(searcher_id, user_id).await {
                Ok(search_record) => {
                    tracing::info!("✨ 创建搜索记录: username={}, user_id={}, search_session_id={}", 
                        user.username, user_id, search_record.search_session_id);
                    results.push(SearchedUser {
                        user_id: user.id,
                        username: user.username.clone(),
                        nickname: user.display_name.clone().unwrap_or_else(|| user.username.clone()),
                        avatar_url: user.avatar_url.clone(),
                        user_type: user.user_type,
                        search_session_id: search_record.search_session_id,
                        is_friend,
                        can_send_message,
                    });
                }
                Err(e) => {
                    tracing::warn!("⚠️ 创建搜索记录失败: {} -> {}: {}", searcher_id, user_id, e);
                    // 继续处理，但使用默认的 search_session_id
                    results.push(SearchedUser {
                        user_id: user.id,
                        username: user.username.clone(),
                        nickname: user.display_name.clone().unwrap_or_else(|| user.username.clone()),
                        avatar_url: user.avatar_url.clone(),
                        user_type: user.user_type,
                        search_session_id: 0, // 搜索记录创建失败时使用默认值
                        is_friend,
                        can_send_message,
                    });
                }
            }
        }
    }
    
    tracing::info!("✅ 用户精确搜索完成: query={}, found={}", query, if results.is_empty() { "否" } else { "是" });
    
    let response = AccountSearchResponse {
        users: results.clone(),
        total: results.len(),
        query: query.to_string(),
    };
    
    Ok(serde_json::to_value(response).unwrap())
}

