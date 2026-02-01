/// 好友服务 - 处理好友关系管理
/// 
/// 提供完整的好友系统功能：
/// - 好友请求发送/接受/拒绝
/// - 好友列表管理
/// - 好友关系状态
/// - entity/sync_entities 业务逻辑（好友分页与 payload 构建）

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use serde_json::json;
use tracing::{info, warn};

use crate::model::friend::*;
use crate::model::privacy::FriendRequestSource;
use crate::error::{Result, ServerError};
use crate::repository::UserRepository;
use crate::infra::CacheManager;

/// 将 FriendRequestSource 转为 (source_type, source_id) 字符串，用于落库与列表返回
fn source_to_strings(source: &Option<FriendRequestSource>) -> (Option<String>, Option<String>) {
    match source.as_ref() {
        Some(FriendRequestSource::Search { search_session_id }) => (
            Some("search".to_string()),
            Some(search_session_id.to_string()),
        ),
        Some(FriendRequestSource::Group { group_id }) => (
            Some("group".to_string()),
            Some(group_id.to_string()),
        ),
        Some(FriendRequestSource::CardShare { share_id }) => (
            Some("card_share".to_string()),
            Some(share_id.to_string()),
        ),
        Some(FriendRequestSource::Qrcode { qrcode }) => (
            Some("qrcode".to_string()),
            Some(qrcode.clone()),
        ),
        Some(FriendRequestSource::Phone { phone }) => (
            Some("phone".to_string()),
            Some(phone.clone()),
        ),
        None => (None, None),
    }
}

/// 好友服务（基于内存存储）
pub struct FriendService {
    /// 好友请求存储：request_id -> FriendRequest
    friend_requests: Arc<RwLock<HashMap<u64, FriendRequest>>>,
    /// 好友关系存储：user_id -> friend_id -> Friendship
    friendships: Arc<RwLock<HashMap<u64, HashMap<u64, Friendship>>>>
}

impl FriendService {
    /// 创建新的好友服务
    pub fn new() -> Self {
        Self {
            friend_requests: Arc::new(RwLock::new(HashMap::new())),
            friendships: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 发送好友请求
    pub async fn send_friend_request(
        &self,
        from_user_id: u64,
        to_user_id: u64,
        message: Option<String>,
    ) -> Result<u64> {
        self.send_friend_request_with_source(from_user_id, to_user_id, message, None).await
    }
    
    /// 发送带来源的好友请求
    pub async fn send_friend_request_with_source(
        &self,
        from_user_id: u64,
        to_user_id: u64,
        message: Option<String>,
        source: Option<crate::model::privacy::FriendRequestSource>,
    ) -> Result<u64> {
        info!("📤 发送好友请求: {} -> {} (source: {:?})", from_user_id, to_user_id, source);
        
        // 创建好友请求
        let request = FriendRequest::new_with_source(
            from_user_id,
            to_user_id,
            message,
            source,
        );
        
        let request_id = request.id;
        
        // 存储好友请求
        self.friend_requests.write().await.insert(request_id, request);
        
        info!("✅ 好友请求已发送: {}", request_id);
        Ok(request_id)
    }
    
    /// 接受好友请求
    pub async fn accept_friend_request(
        &self,
        user_id: u64,
        from_user_id: u64,
    ) -> Result<()> {
        self.accept_friend_request_with_source(user_id, from_user_id).await.map(|_| ())
    }
    
    /// 接受好友请求并返回来源信息
    pub async fn accept_friend_request_with_source(
        &self,
        user_id: u64,
        from_user_id: u64,
    ) -> Result<Option<crate::model::privacy::FriendRequestSource>> {
        info!("✅ 用户 {} 接受来自 {} 的好友请求", user_id, from_user_id);
        
        // 查找待处理的好友请求
        let mut requests = self.friend_requests.write().await;
        let request_opt = requests.values_mut()
            .find(|req| req.from_user_id == from_user_id 
                      && req.to_user_id == user_id 
                      && req.status == FriendshipStatus::Pending);
        
        if let Some(request) = request_opt {
            // 保存来源信息并转为 (source_type, source_id) 写入好友关系
            let source = request.source.clone();
            let (source_str, source_id_str) = source_to_strings(&source);
            
            // 更新请求状态
            request.accept();
            
            // 创建双向好友关系（带来源）
            let mut friendships = self.friendships.write().await;
            
            // user_id -> from_user_id
            let mut friendship1 = Friendship::new(user_id, from_user_id);
            friendship1.update_status(FriendshipStatus::Accepted);
            friendship1.source = source_str.clone();
            friendship1.source_id = source_id_str.clone();
            friendships.entry(user_id)
                .or_insert_with(HashMap::new)
                .insert(from_user_id, friendship1);
            
            // from_user_id -> user_id
            let mut friendship2 = Friendship::new(from_user_id, user_id);
            friendship2.update_status(FriendshipStatus::Accepted);
            friendship2.source = source_str;
            friendship2.source_id = source_id_str;
            friendships.entry(from_user_id)
                .or_insert_with(HashMap::new)
                .insert(user_id, friendship2);
            
            info!("✅ 好友关系已建立: {} <-> {}", user_id, from_user_id);
            Ok(source)
        } else {
            warn!("⚠️ 未找到待处理的好友请求: {} -> {}", from_user_id, user_id);
            Err(ServerError::NotFound(format!("Friend request not found")))
        }
    }
    
    /// 获取好友列表
    pub async fn get_friends(&self, user_id: u64) -> Result<Vec<u64>> {
        info!("📋 获取用户 {} 的好友列表", user_id);
        
        let friendships = self.friendships.read().await;
        if let Some(user_friends) = friendships.get(&user_id) {
            let friend_ids: Vec<u64> = user_friends.iter()
                .filter(|(_, friendship)| friendship.status == FriendshipStatus::Accepted)
                .map(|(friend_id, _)| *friend_id)
                .collect();
            Ok(friend_ids)
        } else {
            Ok(vec![])
        }
    }
    
    /// 删除好友
    pub async fn remove_friend(
        &self,
        user_id: u64,
        friend_id: u64,
    ) -> Result<()> {
        info!("🗑️ 用户 {} 删除好友 {}", user_id, friend_id);
        
        let mut friendships = self.friendships.write().await;
        
        // 删除 user_id -> friend_id 的关系
        if let Some(user_friends) = friendships.get_mut(&user_id) {
            user_friends.remove(&friend_id);
        }
        
        // 删除 friend_id -> user_id 的关系
        if let Some(friend_friends) = friendships.get_mut(&friend_id) {
            friend_friends.remove(&user_id);
        }
        
        info!("✅ 好友关系已删除: {} <-> {}", user_id, friend_id);
        Ok(())
    }
    
    /// 检查是否是好友
    pub async fn is_friend(&self, user_id: u64, friend_id: u64) -> bool {
        let friendships = self.friendships.read().await;
        if let Some(user_friends) = friendships.get(&user_id) {
            if let Some(friendship) = user_friends.get(&friend_id) {
                return friendship.status == FriendshipStatus::Accepted;
            }
        }
        false
    }

    /// 获取与某用户的好友关系（用于列表返回 source_type/source_id）
    pub async fn get_friendship(&self, user_id: u64, friend_id: u64) -> Option<Friendship> {
        let friendships = self.friendships.read().await;
        friendships.get(&user_id).and_then(|m| m.get(&friend_id).cloned())
    }
    
    /// 获取待处理的好友申请列表（接收到的）
    pub async fn get_pending_requests(&self, user_id: u64) -> Result<Vec<FriendRequest>> {
        info!("📋 获取用户 {} 的待处理好友申请列表", user_id);
        
        let requests = self.friend_requests.read().await;
        let pending_requests: Vec<FriendRequest> = requests.values()
            .filter(|req| req.to_user_id == user_id && req.status == FriendshipStatus::Pending)
            .cloned()
            .collect();
        
        Ok(pending_requests)
    }
    
    /// 获取发送的好友申请列表（已发送但未处理）
    pub async fn get_sent_requests(&self, user_id: u64) -> Result<Vec<FriendRequest>> {
        info!("📋 获取用户 {} 已发送的好友申请列表", user_id);
        
        let requests = self.friend_requests.read().await;
        let sent_requests: Vec<FriendRequest> = requests.values()
            .filter(|req| req.from_user_id == user_id && req.status == FriendshipStatus::Pending)
            .cloned()
            .collect();
        
        Ok(sent_requests)
    }

    /// entity/sync_entities 业务逻辑：好友分页与 SyncEntitiesResponse 构建
    pub async fn sync_entities_page(
        &self,
        user_id: u64,
        _since_version: Option<u64>,
        scope: Option<&str>,
        limit: u32,
        user_repository: &Arc<UserRepository>,
        cache_manager: &Arc<CacheManager>,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{SyncEntitiesResponse, SyncEntityItem};
        use crate::rpc::helpers::get_user_profile_with_fallback;

        let friend_ids = self.get_friends(user_id).await?;
        let limit = limit.min(200).max(1);
        let after_id: Option<u64> = scope
            .and_then(|s| s.strip_prefix("cursor:"))
            .and_then(|s| s.parse::<u64>().ok());

        let start_idx = if let Some(aid) = after_id {
            friend_ids.iter().position(|&id| id == aid).map(|i| i + 1).unwrap_or(0)
        } else {
            0
        };
        let friend_ids_page: Vec<u64> = friend_ids.iter().skip(start_idx).take(limit as usize).cloned().collect();
        let total_consumed = start_idx + friend_ids_page.len();
        let has_more = total_consumed < friend_ids.len();

        let mut items = Vec::with_capacity(friend_ids_page.len());
        for friend_id in &friend_ids_page {
            let profile_opt = get_user_profile_with_fallback(*friend_id, user_repository, cache_manager).await.ok().flatten();
            let profile = match profile_opt {
                Some(p) => p,
                None => continue,
            };
            let friendship = self.get_friendship(user_id, *friend_id).await;
            let created_at = friendship
                .as_ref()
                .map(|f| f.created_at.timestamp_millis())
                .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());

            let payload = json!({
                "user": {
                    "username": profile.username,
                    "nickname": profile.nickname,
                    "avatar": profile.avatar_url.as_deref().unwrap_or(""),
                    "user_type": profile.user_type,
                },
                "friend": {
                    "created_at": created_at,
                },
            });

            items.push(SyncEntityItem {
                entity_id: friend_id.to_string(),
                version: 1,
                deleted: false,
                payload: Some(payload),
            });
        }

        Ok(SyncEntitiesResponse {
            items,
            next_version: 1,
            has_more,
            min_version: None,
        })
    }
}
