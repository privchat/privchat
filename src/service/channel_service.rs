use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn, error};
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};
use sqlx;

use crate::model::channel::{
    Channel, ChannelType, ChannelStatus, MemberRole, ChannelMember,
    CreateChannelRequest, ChannelResponse, ChannelListResponse
};
use crate::error::{Result, ServerError};
use crate::repository::{ChannelRepository, PgChannelRepository};

/// 最后消息预览
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastMessagePreview {
    /// 消息 ID
    pub message_id: u64,
    /// 发送者 ID
    pub sender_id: u64,
    /// 消息内容预览（截断到 50 字）
    pub content: String,
    /// 消息类型（用于显示占位符如 "[图片]"）
    pub message_type: String,
    /// 发送时间
    pub timestamp: DateTime<Utc>,
}

/// 增强的会话项（包含预览、未读、置顶等信息）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedChannelItem {
    /// 会话信息
    pub channel: Channel,
    /// 最后消息预览
    pub last_message: Option<LastMessagePreview>,
    /// 未读消息数
    pub unread_count: u64,
    /// 是否置顶
    pub is_pinned: bool,
    /// 是否免打扰
    pub is_muted: bool,
}

/// 增强的会话列表响应
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnhancedChannelListResponse {
    /// 会话列表
    pub channels: Vec<EnhancedChannelItem>,
    /// 总数
    pub total: usize,
    /// 是否还有更多
    pub has_more: bool,
}

/// 会话服务配置
#[derive(Debug, Clone)]
pub struct ChannelServiceConfig {
    /// 单个用户最大会话数
    pub max_channels_per_user: usize,
    /// 群聊最大成员数
    pub max_group_members: usize,
    /// 是否允许创建公开群聊
    pub allow_public_groups: bool,
    /// 会话ID生成前缀
    pub channel_id_prefix: String,
}

impl Default for ChannelServiceConfig {
    fn default() -> Self {
        Self {
            max_channels_per_user: 1000,
            max_group_members: 500,
            allow_public_groups: true,
            channel_id_prefix: "conv_".to_string(),
        }
    }
}

/// 会话服务统计信息
#[derive(Debug, Clone)]
pub struct ChannelServiceStats {
    /// 总会话数
    pub total_channels: usize,
    /// 活跃会话数
    pub active_channels: usize,
    /// 私聊会话数
    pub direct_channels: usize,
    /// 群聊会话数
    pub group_channels: usize,
    /// 系统会话数
    pub system_channels: usize,
    /// 总成员数
    pub total_members: usize,
    /// 平均每个会话的成员数
    pub avg_members_per_channel: f64,
}

/// 频道服务（原会话服务）
pub struct ChannelService {
    /// 配置
    config: ChannelServiceConfig,
    /// 会话仓库（PostgreSQL）
    channel_repository: Arc<PgChannelRepository>,
    /// 会话存储 channel_id -> Channel（内存缓存）
    channels: Arc<RwLock<HashMap<u64, Channel>>>,
    /// 用户会话索引 user_id -> Vec<channel_id>
    user_channels: Arc<RwLock<HashMap<u64, Vec<u64>>>>,
    /// 私聊会话索引 (user1_id, user2_id) -> channel_id
    direct_channel_index: Arc<RwLock<HashMap<(u64, u64), u64>>>,
    /// 置顶会话 (user_id, channel_id) -> is_pinned
    pinned_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 免打扰会话 (user_id, channel_id) -> is_muted
    muted_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 隐藏会话 (user_id, channel_id) -> is_hidden
    hidden_channels: Arc<RwLock<HashSet<(u64, u64)>>>,
    /// 最后消息预览缓存 channel_id -> LastMessagePreview
    last_message_cache: Arc<RwLock<HashMap<u64, LastMessagePreview>>>,
}

impl ChannelService {
    /// 创建新的会话服务
    pub fn new(config: ChannelServiceConfig, channel_repository: Arc<PgChannelRepository>) -> Self {
        Self {
            config,
            channel_repository,
            channels: Arc::new(RwLock::new(HashMap::new())),
            user_channels: Arc::new(RwLock::new(HashMap::new())),
            direct_channel_index: Arc::new(RwLock::new(HashMap::new())),
            pinned_channels: Arc::new(RwLock::new(HashSet::new())),
            muted_channels: Arc::new(RwLock::new(HashSet::new())),
            hidden_channels: Arc::new(RwLock::new(HashSet::new())),
            last_message_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// 使用默认配置创建会话服务（需要 Repository）
    pub fn new_with_repository(channel_repository: Arc<PgChannelRepository>) -> Self {
        Self::new(ChannelServiceConfig::default(), channel_repository)
    }

    /// 使用默认配置创建会话服务（向后兼容，但会警告）
    #[deprecated(note = "使用 new_with_repository 替代，以支持数据库持久化")]
    pub fn new_default() -> Self {
        // 创建一个临时的 Repository（这不应该在生产环境使用）
        // 注意：这会导致编译错误，因为需要 PgPool
        // 保留此方法仅用于向后兼容，实际应该使用 new_with_repository
        panic!("new_default() 已废弃，请使用 new_with_repository()")
    }

    /// 获取数据库连接池（用于事务操作）
    pub fn pool(&self) -> &sqlx::PgPool {
        self.channel_repository.pool()
    }
    
    // =====================================================
    // 管理 API 方法
    // =====================================================
    
    /// 获取群组列表（管理 API）
    pub async fn list_groups_admin(&self, page: u32, page_size: u32) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;
        
        let groups = sqlx::query!(
            r#"
            SELECT 
                c.channel_id,
                c.group_id,
                g.name as group_name,
                g.description,
                g.owner_id as owner_id,
                g.member_count,
                COALESCE(g.created_at, 0) as group_created_at,
                COALESCE(g.updated_at, 0) as group_updated_at,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at as channel_created_at,
                c.updated_at as channel_updated_at
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            WHERE c.channel_type = 1
            ORDER BY c.updated_at DESC
            LIMIT $1 OFFSET $2
            "#,
            page_size as i64,
            offset as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组列表失败: {}", e)))?;
        
        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 1
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计群组数失败: {}", e)))?;
        
        let total = total_result.count.unwrap_or(0) as u32;
        
        let group_list: Vec<serde_json::Value> = groups.into_iter().map(|g| {
            let owner_id = Some(g.owner_id as u64);
            serde_json::json!({
                "group_id": g.group_id.unwrap_or(0) as u64,
                "channel_id": g.channel_id as u64,
                "name": g.group_name,
                "description": g.description,
                "owner_id": owner_id,
                "member_count": g.member_count.unwrap_or(0) as u32,
                "last_message_id": g.last_message_id.map(|id| id as u64),
                "last_message_at": g.last_message_at.map(|ts| ts),
                "message_count": g.message_count.unwrap_or(0) as u64,
                "created_at": g.group_created_at,
                "updated_at": g.group_updated_at,
            })
        }).collect();
        
        Ok((group_list, total))
    }
    
    /// 获取群组详情（管理 API）
    pub async fn get_group_admin(&self, group_id: u64) -> Result<serde_json::Value> {
        let group = sqlx::query!(
            r#"
            SELECT 
                c.channel_id,
                c.group_id,
                g.name as group_name,
                g.description,
                g.owner_id as owner_id,
                g.member_count,
                COALESCE(g.created_at, 0) as group_created_at,
                COALESCE(g.updated_at, 0) as group_updated_at,
                c.last_message_id,
                c.last_message_at,
                c.message_count,
                c.created_at as channel_created_at,
                c.updated_at as channel_updated_at
            FROM privchat_channels c
            LEFT JOIN privchat_groups g ON c.group_id = g.group_id
            WHERE c.channel_type = 1 AND c.group_id = $1
            LIMIT 1
            "#,
            group_id as i64,
        )
        .fetch_optional(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组详情失败: {}", e)))?;
        
        let group = group.ok_or_else(|| ServerError::NotFound(format!("群组 {} 不存在", group_id)))?;
        
        // 查询群组成员
        let members = sqlx::query!(
            r#"
            SELECT 
                user_id,
                role,
                joined_at,
                nickname
            FROM privchat_group_members
            WHERE group_id = $1
            ORDER BY joined_at ASC
            "#,
            group_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询群组成员失败: {}", e)))?;
        
        let member_list: Vec<serde_json::Value> = members.into_iter().map(|m| serde_json::json!({
            "user_id": m.user_id as u64,
            "role": m.role,
            "joined_at": m.joined_at,
            "nickname": m.nickname,
        })).collect();
        
        let owner_id = Some(group.owner_id as u64);
        Ok(serde_json::json!({
            "group_id": group_id,
            "channel_id": group.channel_id as u64,
            "name": group.group_name,
            "description": group.description,
            "owner_id": owner_id,
            "member_count": group.member_count.unwrap_or(0) as u32,
            "members": member_list,
            "last_message_id": group.last_message_id.map(|id| id as u64),
            "last_message_at": group.last_message_at.map(|ts| ts),
            "message_count": group.message_count.unwrap_or(0) as u64,
            "created_at": group.group_created_at,
            "updated_at": group.group_updated_at,
        }))
    }
    
    /// 解散群组（管理 API）
    pub async fn dissolve_group_admin(&self, group_id: u64) -> Result<()> {
        let mut tx = self.pool().begin().await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;
        
        // 检查群组是否存在
        let exists: Option<(i32,)> = sqlx::query_as(
            r#"
            SELECT 1
            FROM privchat_channels
            WHERE channel_type = 1 AND group_id = $1
            LIMIT 1
            "#
        )
        .bind(group_id as i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("检查群组失败: {}", e)))?;
        
        if exists.is_none() {
            return Err(ServerError::NotFound(format!("群组 {} 不存在", group_id)));
        }
        
        // 删除群组成员
        sqlx::query!(
            r#"
            DELETE FROM privchat_group_members
            WHERE group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组成员失败: {}", e)))?;
        
        // 删除群组会话
        sqlx::query!(
            r#"
            DELETE FROM privchat_channels
            WHERE channel_type = 1 AND group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组会话失败: {}", e)))?;
        
        // 删除群组记录
        sqlx::query!(
            r#"
            DELETE FROM privchat_groups
            WHERE group_id = $1
            "#,
            group_id as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("删除群组记录失败: {}", e)))?;
        
        tx.commit().await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;
        
        info!("✅ 群组已解散: group_id={}", group_id);
        Ok(())
    }
    
    /// 获取好友关系列表（管理 API，通过私聊会话推断）
    pub async fn list_friendships_admin(&self, page: u32, page_size: u32) -> Result<(Vec<serde_json::Value>, u32)> {
        let offset = (page - 1) * page_size;
        
        let channels = sqlx::query!(
            r#"
            SELECT 
                channel_id,
                direct_user1_id,
                direct_user2_id,
                created_at,
                updated_at,
                last_message_at
            FROM privchat_channels
            WHERE channel_type = 0
            ORDER BY updated_at DESC
            LIMIT $1 OFFSET $2
            "#,
            page_size as i64,
            offset as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询好友关系失败: {}", e)))?;
        
        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 0
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计好友关系数失败: {}", e)))?;
        
        let total = total_result.count.unwrap_or(0) as u32;
        
        let friendship_list: Vec<serde_json::Value> = channels.into_iter().map(|c| serde_json::json!({
            "channel_id": c.channel_id as u64,
            "user1_id": c.direct_user1_id.unwrap_or(0) as u64,
            "user2_id": c.direct_user2_id.unwrap_or(0) as u64,
            "created_at": c.created_at,
            "updated_at": c.updated_at,
            "last_message_at": c.last_message_at,
        })).collect();
        
        Ok((friendship_list, total))
    }
    
    /// 获取用户的好友列表（管理 API，通过私聊会话推断）
    pub async fn get_user_friends_admin(&self, user_id: u64) -> Result<Vec<u64>> {
        let channels = sqlx::query!(
            r#"
            SELECT 
                direct_user1_id,
                direct_user2_id
            FROM privchat_channels
            WHERE channel_type = 0
              AND (direct_user1_id = $1 OR direct_user2_id = $1)
            "#,
            user_id as i64,
        )
        .fetch_all(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("查询用户好友列表失败: {}", e)))?;
        
        let mut friend_ids = Vec::new();
        for conv in channels {
            let friend_id = if conv.direct_user1_id.unwrap_or(0) as u64 == user_id {
                conv.direct_user2_id.unwrap_or(0) as u64
            } else {
                conv.direct_user1_id.unwrap_or(0) as u64
            };
            friend_ids.push(friend_id);
        }
        
        Ok(friend_ids)
    }
    
    /// 获取群组统计（管理 API）
    pub async fn get_group_stats_admin(&self) -> Result<serde_json::Value> {
        let total_result = sqlx::query!(
            r#"
            SELECT COUNT(*) as count
            FROM privchat_channels
            WHERE channel_type = 1
            "#
        )
        .fetch_one(self.pool())
        .await
        .map_err(|e| ServerError::Database(format!("统计群组数失败: {}", e)))?;
        
        let total = total_result.count.unwrap_or(0) as u32;
        
        // TODO: 实现活跃群组和已解散群组的统计
        Ok(serde_json::json!({
            "total": total,
            "active": total,  // TODO: 实现活跃群组统计
            "dissolved": 0,   // TODO: 实现已解散群组统计
        }))
    }
    
    /// 创建好友关系（管理 API）
    /// 
    /// 让两个用户成为好友，并自动创建私聊会话
    pub async fn create_friendship_admin(
        &self,
        user1_id: u64,
        user2_id: u64,
        channel_service: &crate::service::ChannelService,
    ) -> Result<u64> {
        if user1_id == user2_id {
            return Err(ServerError::Validation("不能添加自己为好友".to_string()));
        }
        
        // 确定较小的用户ID在前（用于数据库查询）
        let (smaller_id, larger_id) = if user1_id < user2_id {
            (user1_id, user2_id)
        } else {
            (user2_id, user1_id)
        };
        
        let pool = self.pool();
        let mut tx = pool.begin().await
            .map_err(|e| ServerError::Database(format!("开启事务失败: {}", e)))?;
        
        // 检查是否已存在私聊会话
        let existing_channel: Option<(i64,)> = sqlx::query_as(
            r#"
            SELECT channel_id
            FROM privchat_channels
            WHERE channel_type = 0
              AND direct_user1_id = $1
              AND direct_user2_id = $2
            LIMIT 1
            "#
        )
        .bind(smaller_id as i64)
        .bind(larger_id as i64)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("查询会话失败: {}", e)))?;
        
        let channel_id = if let Some((id,)) = existing_channel {
            info!("✅ 私聊会话已存在: {}", id);
            id as u64
        } else {
            // 创建新的私聊会话
            let now = chrono::Utc::now().timestamp_millis();
            let result = sqlx::query!(
                r#"
                INSERT INTO privchat_channels (
                    channel_type, direct_user1_id, direct_user2_id,
                    created_at, updated_at
                )
                VALUES (0, $1, $2, $3, $3)
                RETURNING channel_id
                "#,
                smaller_id as i64,
                larger_id as i64,
                now
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("创建会话失败: {}", e)))?;
            
            let channel_id = result.channel_id as u64;
            
            // 创建 Channel（内存操作）
            if let Err(e) = channel_service.create_private_chat_with_id(
                user1_id,
                user2_id,
                channel_id,
            ).await {
                warn!("⚠️ 创建私聊频道失败: {}，频道可能已存在", e);
            }
            
            channel_id
        };
        
        // 提交事务
        tx.commit().await
            .map_err(|e| ServerError::Database(format!("提交事务失败: {}", e)))?;
        
        info!("✅ 会话创建成功: channel_id={}", channel_id);
        Ok(channel_id)
    }
    
    /// 生成私聊会话的索引键（已排序，确保一致性）
    fn make_direct_key(user1_id: u64, user2_id: u64) -> (u64, u64) {
        if user1_id < user2_id {
            (user1_id, user2_id)
        } else {
            (user2_id, user1_id)
        }
    }

    /// 创建会话
    pub async fn create_channel(
        &self,
        creator_id: u64,
        request: CreateChannelRequest,
    ) -> Result<ChannelResponse> {
        tracing::info!("🔧 create_channel 开始: creator_id={}, type={:?}, member_ids={:?}", 
                      creator_id, request.channel_type, request.member_ids);
        
        // 验证请求
        if let Err(error) = self.validate_create_request(&creator_id, &request).await {
            tracing::error!("❌ 验证失败: {}", error);
            return Ok(ChannelResponse {
                channel: Channel::new_direct(0, 0, 0),
                success: false,
                error: Some(error),
            });
        }
        
        tracing::info!("✅ 验证通过");

        // 注意：channel_id 应该由数据库自动生成（BIGSERIAL）
        // 这里先使用0作为占位符，实际ID会在数据库插入后返回
        let channel_id = 0;  // 需要从数据库获取
        
        let mut channel = match request.channel_type {
            ChannelType::Direct => {
                // 私聊必须有且仅有一个目标用户
                if request.member_ids.len() != 1 {
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some("Direct channel must have exactly one target user".to_string()),
                    });
                }

                let target_user_id = request.member_ids[0];
                
                // 检查是否已存在私聊会话
                if let Some(existing_id) = self.find_direct_channel(creator_id, target_user_id).await {
                    let channels = self.channels.read().await;
                    if let Some(existing_conv) = channels.get(&existing_id) {
                        return Ok(ChannelResponse {
                            channel: existing_conv.clone(),
                            success: true,
                            error: None,
                        });
                    }
                }

                Channel::new_direct(channel_id, creator_id, target_user_id)
            }
            ChannelType::Group => {
                // 群聊需要名称
                if request.name.is_none() {
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some("Group channel must have a name".to_string()),
                    });
                }

                // ✨ 先创建 privchat_groups 记录，获取 group_id
                let now = chrono::Utc::now().timestamp_millis();
                let group_name = request.name.clone().unwrap();
                let group_description = request.description.clone();
                
                let group_id = match sqlx::query_as::<_, (i64,)>(
                    r#"
                    INSERT INTO privchat_groups (
                        name, description, owner_id, member_count, created_at, updated_at
                    )
                    VALUES ($1, $2, $3, 1, $4, $4)
                    RETURNING group_id
                    "#
                )
                .bind(&group_name)
                .bind(group_description.as_deref())
                .bind(creator_id as i64)
                .bind(now)
                .fetch_one(self.channel_repository.pool())
                .await
                {
                    Ok(row) => row.0 as u64,
                    Err(e) => {
                        error!("❌ 创建群组记录失败: {}", e);
                        return Ok(ChannelResponse {
                            channel: Channel::new_direct(0, 0, 0),
                            success: false,
                            error: Some(format!("创建群组记录失败: {}", e)),
                        });
                    }
                };
                
                info!("✅ 群组记录已创建: group_id={}", group_id);
                
                // 使用 group_id 作为 channel_id 创建会话
                let mut conv = Channel::new_group(group_id, creator_id, request.name.clone());
                
                // 设置群聊属性
                conv.metadata.description = request.description;
                conv.metadata.is_public = request.is_public.unwrap_or(false);
                conv.metadata.max_members = request.max_members.or(Some(self.config.max_group_members));

                // 添加初始成员
                for member_id in request.member_ids {
                    if let Err(error) = conv.add_member(member_id, None) {
                        warn!("Failed to add member to group: {}", error);
                    }
                }

                conv
            }
            ChannelType::System => {
                return Ok(ChannelResponse {
                    channel: Channel::new_direct(0, 0, 0),
                    success: false,
                    error: Some("System channels cannot be created by users".to_string()),
                });
            }
        };

        // ✨ 保存会话到数据库
        // 如果是群聊，使用 create_channel_with_id（因为 group_id 已经创建）
        // 如果是私聊，使用 create（让数据库自动生成 channel_id）
        tracing::info!("🔧 准备保存会话到数据库: type={:?}, id={}", channel.channel_type, channel.id);
        
        let created_channel = if channel.channel_type == ChannelType::Group {
            // 群聊：使用指定的 channel_id（等于 group_id）
            match self.channel_repository.create(&channel).await {
                Ok(created) => {
                    info!("✅ 群聊会话已保存到数据库: {}", created.id);
                    created
                }
                Err(e) => {
                    error!("❌ 保存群聊会话到数据库失败: {}", e);
                    // 如果创建失败，删除已创建的群组记录
                    if let Some(group_id) = channel.group_id {
                        let _ = sqlx::query("DELETE FROM privchat_groups WHERE group_id = $1")
                            .bind(group_id as i64)
                            .execute(self.channel_repository.pool())
                            .await;
                    }
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("创建会话失败: {}", e)),
                    });
                }
            }
        } else {
            // 私聊：让数据库自动生成 channel_id
            tracing::info!("🔧 调用 repository.create() 创建私聊会话");
            match self.channel_repository.create(&channel).await {
                Ok(created) => {
                    info!("✅ 私聊会话已保存到数据库: id={}", created.id);
                    created
                }
                Err(e) => {
                    error!("❌ 保存私聊会话到数据库失败: {}", e);
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some(format!("创建会话失败: {}", e)),
                    });
                }
            }
        };
        
        let actual_conv_id = created_channel.id;

        // ✨ 保存成员信息到数据库（因为 channel_repository.create 返回的 channel 没有成员信息）
        // 使用原始的 channel 对象中的成员信息
        for (member_id, member) in &channel.members {
            if let Err(e) = self.channel_repository.add_participant(
                actual_conv_id,
                *member_id,
                member.role,
            ).await {
                warn!("⚠️ 保存成员到数据库失败: channel_id={}, user_id={}, error={}", 
                      actual_conv_id, member_id, e);
            }
        }

        // ✨ 更新 channel 的 ID（如果数据库自动生成了新的 ID）
        channel.id = actual_conv_id;

        // 存储会话到内存缓存（使用包含成员的原始 channel 对象）
        let mut channels = self.channels.write().await;
        channels.insert(actual_conv_id, channel.clone());
        drop(channels);

        // 更新用户会话索引（使用包含成员的原始 channel 对象）
        self.update_user_channel_index(&channel).await;

        // 如果是私聊，更新私聊索引（使用包含成员的原始 channel 对象）
        if channel.channel_type == ChannelType::Direct {
            let member_ids = channel.get_member_ids();
            if member_ids.len() == 2 {
                let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                let mut direct_index = self.direct_channel_index.write().await;
                direct_index.insert(key, actual_conv_id);
            }
        }

        info!(
            "Created channel: {} (type: {:?}, creator: {}, members: {})",
            actual_conv_id,
            channel.channel_type,
            creator_id,
            channel.members.len()
        );

        Ok(ChannelResponse {
            channel,
            success: true,
            error: None,
        })
    }

    /// 使用指定ID创建会话（用于系统内部创建会话）
    pub async fn create_channel_with_id(
        &self,
        channel_id: u64,
        creator_id: u64,
        request: CreateChannelRequest,
    ) -> Result<ChannelResponse> {
        // ✨ 修复：检查会话是否已存在（内存和数据库）
        {
            let channels = self.channels.read().await;
            if let Some(existing_conv) = channels.get(&channel_id) {
                // 会话在内存中存在，但还需要确保数据库中也有记录
                // 尝试从数据库查询，如果不存在则创建
                if self.channel_repository.find_by_id(channel_id).await.ok().flatten().is_none() {
                    warn!("⚠️ 会话 {} 在内存中存在但数据库中不存在，尝试补充创建", channel_id);
                    // 保存到数据库
                    if let Err(e) = self.channel_repository.create(&existing_conv).await {
                        warn!("⚠️ 补充创建会话到数据库失败: {}", e);
                    } else {
                        info!("✅ 已补充创建会话到数据库: {}", channel_id);
                        // 同时保存参与者
                        for (member_id, member) in &existing_conv.members {
                            let _ = self.channel_repository.add_participant(
                                channel_id,
                                *member_id,
                                member.role,
                            ).await;
                        }
                    }
                }
                return Ok(ChannelResponse {
                    channel: existing_conv.clone(),
                    success: true,
                    error: None,
                });
            }
        }
        
        // 验证请求
        if let Err(error) = self.validate_create_request(&creator_id, &request).await {
            return Ok(ChannelResponse {
                channel: Channel::new_direct(0, 0, 0),
                success: false,
                error: Some(error),
            });
        }
        
        let channel = match request.channel_type {
            ChannelType::Direct => {
                // 私聊必须有且仅有一个目标用户
                if request.member_ids.len() != 1 {
                    return Ok(ChannelResponse {
                        channel: Channel::new_direct(0, 0, 0),
                        success: false,
                        error: Some("Direct channel must have exactly one target user".to_string()),
                    });
                }

                let target_user_id = request.member_ids[0];
                Channel::new_direct(channel_id, creator_id, target_user_id)
            }
            ChannelType::Group => {
                let mut conv = Channel::new_group(channel_id, creator_id, request.name.clone());
                
                // ✨ 先在 privchat_groups 表中创建群组记录，然后设置 group_id
                // 注意：现在使用 u64 类型，直接使用 channel_id 作为 group_id
                let now = chrono::Utc::now().timestamp_millis();
                
                // 在 privchat_groups 表中创建群组记录
                let group_created = match sqlx::query(
                    r#"
                    INSERT INTO privchat_groups (
                        group_id, name, description, owner_id, member_count, created_at, updated_at
                    )
                    VALUES ($1, $2, $3, $4, 1, $5, $5)
                    ON CONFLICT (group_id) DO NOTHING
                    "#
                )
                .bind(channel_id as i64)  // PostgreSQL BIGINT
                .bind(request.name.as_ref().unwrap_or(&"未命名群组".to_string()))
                .bind(request.description.as_ref())
                .bind(creator_id as i64)  // PostgreSQL BIGINT
                .bind(now)
                .execute(self.channel_repository.pool())
                .await {
                    Ok(_) => {
                        info!("✅ 群组记录已创建: {}", channel_id);
                        true
                    }
                    Err(e) => {
                        // 如果失败（通常是 owner_id 外键约束失败），记录警告
                        warn!("⚠️ 创建群组记录失败: {}，可能是用户不存在于数据库中", e);
                        // 不设置 group_id，让会话创建时也失败，这样错误更明确
                        false
                    }
                };
                
                // 只有群组记录创建成功时才设置 group_id
                if group_created {
                    conv.group_id = Some(channel_id);
                } else {
                    // 如果群组记录创建失败，返回错误
                    return Ok(ChannelResponse {
                        channel: conv,
                        success: false,
                        error: Some(format!("无法创建群组记录：用户 {} 可能不存在于数据库中", creator_id)),
                    });
                }
                
                // 设置群聊属性
                conv.metadata.description = request.description;
                conv.metadata.is_public = request.is_public.unwrap_or(false);
                conv.metadata.max_members = request.max_members.or(Some(self.config.max_group_members));

                // 添加初始成员
                for member_id in &request.member_ids {
                    let _ = conv.add_member(*member_id, None);
                }

                conv
            }
            ChannelType::System => {
                // 系统会话通常是只读的，用于系统通知
                let mut conv = Channel::new_group(channel_id, creator_id, request.name.clone());
                conv.metadata.description = request.description;
                conv
            }
        };

        // ✨ 保存会话到数据库
        if let Err(e) = self.channel_repository.create(&channel).await {
            error!("❌ 保存会话到数据库失败: {}", e);
            // 继续执行，但记录错误（降级到内存存储）
        } else {
            info!("✅ 会话已保存到数据库: {}", channel_id);
            
            // ✨ 保存所有成员到数据库的参与者表
            for (member_id, member) in &channel.members {
                if let Err(e) = self.channel_repository.add_participant(
                    channel_id,
                    *member_id,
                    member.role,
                ).await {
                    warn!("⚠️ 添加参与者 {} 到数据库失败: {}", member_id, e);
                }
            }
        }
        
        // 保存会话到内存缓存
        let mut channels = self.channels.write().await;
        channels.insert(channel_id, channel.clone());
        drop(channels);

        // 更新用户会话索引
        let mut user_channels = self.user_channels.write().await;
        for member_id in channel.members.keys() {
            user_channels.entry(*member_id)
                .or_insert_with(Vec::new)
                .push(channel_id);
        }
        drop(user_channels);

        // 如果是私聊，更新私聊索引
        if channel.channel_type == ChannelType::Direct {
            let member_ids: Vec<u64> = channel.members.keys().cloned().collect();
            if member_ids.len() == 2 {
                let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                let mut index = self.direct_channel_index.write().await;
                index.insert(key, channel_id);
            }
        }

        info!(
            "Created channel with ID: {}, type: {:?}, creator: {}, members: {}",
            channel_id,
            channel.channel_type,
            creator_id,
            channel.members.len()
        );

        Ok(ChannelResponse {
            channel,
            success: true,
            error: None,
        })
    }

    /// 获取会话（返回 Option，用于可选场景）
    pub async fn get_channel_opt(&self, channel_id: u64) -> Option<Channel> {
        // 先从内存缓存获取
        {
            let channels = self.channels.read().await;
            if let Some(conv) = channels.get(&channel_id) {
                return Some(conv.clone());
            }
        }
        
        // 如果内存缓存中没有，从数据库查询
        match self.channel_repository.find_by_id(channel_id).await {
            Ok(Some(conv)) => {
                // 加载参与者信息
                let participants = self.get_channel_participants(channel_id).await
                    .unwrap_or_default();
                
                // 将参与者添加到会话成员中
                let mut conv_with_members = conv.clone();
                for participant in participants {
                    // 直接使用 participant 的 user_id (u64) 创建 ChannelMember
                    let member = ChannelMember::new(participant.user_id, participant.role);
                    conv_with_members.members.insert(participant.user_id, member);
                }
                
                // 缓存到内存
                let mut channels = self.channels.write().await;
                channels.insert(channel_id, conv_with_members.clone());
                
                Some(conv_with_members)
            }
            Ok(None) => None,
            Err(e) => {
                warn!("查询会话失败: {}", e);
                None
            }
        }
    }

    /// 获取会话（返回 Result，用于需要错误处理的场景）
    pub async fn get_channel(&self, channel_id: &u64) -> std::result::Result<Channel, ServerError> {
        self.get_channel_opt(*channel_id).await
            .ok_or_else(|| ServerError::NotFound(format!("Channel not found: {}", channel_id)))
    }

    /// 获取会话参与者（从数据库查询）
    pub async fn get_channel_participants(&self, channel_id: u64) -> std::result::Result<Vec<crate::model::ChannelParticipant>, crate::error::DatabaseError> {
        self.channel_repository.get_participants(channel_id).await
    }
    
    /// 添加参与者到数据库
    pub async fn add_participant(&self, channel_id: u64, user_id: u64, role: crate::model::channel::MemberRole) -> std::result::Result<(), crate::error::DatabaseError> {
        self.channel_repository.add_participant(channel_id, user_id, role).await
    }

    /// 获取用户的所有会话
    pub async fn get_user_channels(&self, user_id: u64) -> ChannelListResponse {
        let user_channels = self.user_channels.read().await;
        let channel_ids = user_channels.get(&user_id).cloned().unwrap_or_default();
        drop(user_channels);

        let channels_lock = self.channels.read().await;
        let mut channels = Vec::new();
        
        for conv_id in channel_ids {
            if let Some(conv) = channels_lock.get(&conv_id) {
                if conv.is_active() {
                    channels.push(conv.clone());
                }
            }
        }
        drop(channels_lock);

        // 按最后消息时间排序
        channels.sort_by(|a, b| {
            b.last_message_at.cmp(&a.last_message_at)
        });

        ChannelListResponse {
            total: channels.len(),
            has_more: false,
            channels,
        }
    }

    /// 获取用户与系统用户的私聊会话 ID（用于发送欢迎等系统消息）。
    /// 若用户尚未有系统会话则返回 None（由调用方决定是否创建）。
    pub async fn get_system_channel_id_for_user(&self, user_id: u64) -> Option<u64> {
        use crate::config::SYSTEM_USER_ID;
        let response = self.get_user_channels(user_id).await;
        for ch in response.channels {
            if ch.channel_type == ChannelType::Direct && ch.get_member_ids().contains(&SYSTEM_USER_ID) {
                return Some(ch.id);
            }
        }
        None
    }

    /// entity/sync_entities 业务逻辑：群组分页与 SyncEntitiesResponse 构建
    pub async fn sync_entities_page_for_groups(
        &self,
        user_id: u64,
        _since_version: Option<u64>,
        scope: Option<&str>,
        limit: u32,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{SyncEntitiesResponse, SyncEntityItem};
        use serde_json::json;

        let response = self.get_user_channels(user_id).await;
        let channels: Vec<Channel> = response
            .channels
            .into_iter()
            .filter(|ch| ch.channel_type == ChannelType::Group)
            .collect();

        let limit = limit.min(200).max(1);
        let after_id: Option<u64> = scope
            .and_then(|s| s.strip_prefix("cursor:"))
            .and_then(|s| s.parse::<u64>().ok());

        let start_idx = if let Some(aid) = after_id {
            channels.iter().position(|ch| ch.id == aid).map(|i| i + 1).unwrap_or(0)
        } else {
            0
        };
        let page: Vec<&Channel> = channels.iter().skip(start_idx).take(limit as usize).collect();
        let total_consumed = start_idx + page.len();
        let has_more = total_consumed < channels.len();

        let items: Vec<SyncEntityItem> = page
            .iter()
            .map(|ch| {
                let payload = json!({
                    "group_id": ch.id,
                    "name": ch.metadata.name.as_deref().unwrap_or(""),
                    "description": ch.metadata.description,
                    "avatar_url": ch.metadata.avatar_url,
                    "owner_id": ch.creator_id,
                    "member_count": ch.members.len() as u32,
                    "created_at": ch.created_at.to_rfc3339(),
                    "updated_at": ch.updated_at.to_rfc3339(),
                });
                SyncEntityItem {
                    entity_id: ch.id.to_string(),
                    version: 1,
                    deleted: false,
                    payload: Some(payload),
                }
            })
            .collect();

        Ok(SyncEntitiesResponse {
            items,
            next_version: 1,
            has_more,
            min_version: None,
        })
    }

    /// entity/sync_entities 业务逻辑：会话列表分页与 SyncEntitiesResponse 构建（私聊+群聊+系统）
    pub async fn sync_entities_page_for_channels(
        &self,
        user_id: u64,
        _since_version: Option<u64>,
        scope: Option<&str>,
        limit: u32,
    ) -> Result<privchat_protocol::rpc::sync::SyncEntitiesResponse> {
        use privchat_protocol::rpc::sync::{SyncEntitiesResponse, SyncEntityItem};
        use serde_json::json;

        let response = self.get_user_channels(user_id).await;
        let channels: Vec<Channel> = response.channels;

        let limit = limit.min(200).max(1);
        let after_id: Option<u64> = scope
            .and_then(|s| s.strip_prefix("cursor:"))
            .and_then(|s| s.parse::<u64>().ok());

        let start_idx = if let Some(aid) = after_id {
            channels.iter().position(|ch| ch.id == aid).map(|i| i + 1).unwrap_or(0)
        } else {
            0
        };
        let page: Vec<&Channel> = channels.iter().skip(start_idx).take(limit as usize).collect();
        let total_consumed = start_idx + page.len();
        let has_more = total_consumed < channels.len();

        let items: Vec<SyncEntityItem> = page
            .iter()
            .map(|ch| {
                let payload = json!({
                    "channel_type": ch.channel_type.to_i16() as i64,
                    "type": ch.channel_type.to_i16() as i64,
                    "channel_name": ch.metadata.name.as_deref().unwrap_or(""),
                    "name": ch.metadata.name.as_deref().unwrap_or(""),
                    "avatar": ch.metadata.avatar_url.as_deref().unwrap_or(""),
                    "unread_count": 0i64,
                });
                SyncEntityItem {
                    entity_id: ch.id.to_string(),
                    version: 1,
                    deleted: false,
                    payload: Some(payload),
                }
            })
            .collect();

        Ok(SyncEntitiesResponse {
            items,
            next_version: 1,
            has_more,
            min_version: None,
        })
    }

    /// 加入会话
    pub async fn join_channel(
        &self,
        channel_id: u64,
        user_id: u64,
        role: Option<MemberRole>,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            // 检查会话状态
            if !channel.is_active() {
                return Ok(false);
            }

            // 检查是否允许加入
            if !channel.metadata.allow_invite && channel.channel_type == ChannelType::Group {
                return Ok(false);
            }

            // 添加成员
            match channel.add_member(user_id, role) {
                Ok(_) => {
                    drop(channels);
                    
                    // 更新用户会话索引
                    let mut user_channels = self.user_channels.write().await;
                    user_channels.entry(user_id)
                        .or_insert_with(Vec::new)
                        .push(channel_id);
                    
                    info!("User {} joined channel {}", user_id, channel_id);
                    Ok(true)
                }
                Err(error) => {
                    warn!("Failed to add user {} to channel {}: {}", user_id, channel_id, error);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// 离开会话
    pub async fn leave_channel(&self, channel_id: u64, user_id: u64) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            match channel.remove_member(&user_id) {
                Ok(_) => {
                    drop(channels);
                    
                    // 更新用户会话索引
                    let mut user_channels = self.user_channels.write().await;
                    if let Some(conv_list) = user_channels.get_mut(&user_id) {
                        conv_list.retain(|id| *id != channel_id);
                    }
                    
                    info!("User {} left channel {}", user_id, channel_id);
                    Ok(true)
                }
                Err(error) => {
                    warn!("Failed to remove user {} from channel {}: {}", user_id, channel_id, error);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// 更新会话信息
    pub async fn update_channel_info(
        &self,
        channel_id: u64,
        user_id: u64,
        name: Option<String>,
        description: Option<String>,
        avatar_url: Option<String>,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            // 检查权限
            if !channel.check_permission(&user_id, |perms| perms.can_edit_info) {
                return Ok(false);
            }

            // 更新信息
            if let Some(name) = name {
                channel.metadata.name = Some(name);
            }
            if let Some(description) = description {
                channel.metadata.description = Some(description);
            }
            if let Some(avatar_url) = avatar_url {
                channel.metadata.avatar_url = Some(avatar_url);
            }

            channel.updated_at = chrono::Utc::now();
            
            info!("Updated channel {} info by user {}", channel_id, user_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 更新成员角色
    pub async fn update_member_role(
        &self,
        channel_id: u64,
        operator_id: u64,
        target_user_id: u64,
        new_role: MemberRole,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            // 检查操作者权限
            if !channel.check_permission(&operator_id, |perms| perms.can_manage_permissions) {
                return Ok(false);
            }

            match channel.update_member_role(&target_user_id, new_role) {
                Ok(_) => {
                    info!("Updated member {} role in channel {} by {}", target_user_id, channel_id, operator_id);
                    Ok(true)
                }
                Err(error) => {
                    warn!("Failed to update member role: {}", error);
                    Ok(false)
                }
            }
        } else {
            Ok(false)
        }
    }

    /// 标记消息已读（单条，兼容旧 RPC）
    pub async fn mark_message_read(
        &self,
        channel_id: u64,
        user_id: u64,
        message_id: u64,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            match channel.mark_member_read(&user_id, message_id) {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            }
        } else {
            Ok(false)
        }
    }

    /// 按 pts 推进已读（正确模型，O(1)）：last_read_pts = max(last_read_pts, read_pts)
    /// 返回更新后的 last_read_pts（若用户不在频道返回 None）
    pub async fn mark_read_pts(
        &self,
        user_id: &u64,
        channel_id: &u64,
        read_pts: u64,
    ) -> Result<Option<u64>> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(member) = channel.members.get_mut(user_id) {
                member.mark_read_pts(read_pts);
                return Ok(Some(member.last_read_pts));
            }
        }
        Ok(None)
    }

    /// 更新最后消息
    pub async fn update_last_message(
        &self,
        channel_id: u64,
        message_id: u64,
    ) -> Result<bool> {
        let mut channels = self.channels.write().await;
        
        if let Some(channel) = channels.get_mut(&channel_id) {
            channel.update_last_message(message_id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// 查找私聊会话
    pub async fn find_direct_channel(&self, user1_id: u64, user2_id: u64) -> Option<u64> {
        let key = Self::make_direct_key(user1_id, user2_id);
        let direct_index = self.direct_channel_index.read().await;
        direct_index.get(&key).copied()
    }

    /// 获取统计信息
    pub async fn get_stats(&self) -> ChannelServiceStats {
        let channels = self.channels.read().await;
        
        let total_channels = channels.len();
        let mut active_channels = 0;
        let mut direct_channels = 0;
        let mut group_channels = 0;
        let mut system_channels = 0;
        let mut total_members = 0;

        for conv in channels.values() {
            if conv.is_active() {
                active_channels += 1;
            }

            match conv.channel_type {
                ChannelType::Direct => direct_channels += 1,
                ChannelType::Group => group_channels += 1,
                ChannelType::System => system_channels += 1,
            }

            total_members += conv.members.len();
        }

        let avg_members_per_channel = if total_channels > 0 {
            total_members as f64 / total_channels as f64
        } else {
            0.0
        };

        ChannelServiceStats {
            total_channels,
            active_channels,
            direct_channels,
            group_channels,
            system_channels,
            total_members,
            avg_members_per_channel,
        }
    }

    /// 验证创建请求
    async fn validate_create_request(
        &self,
        creator_id: &u64,
        request: &CreateChannelRequest,
    ) -> std::result::Result<(), String> {
        // 检查用户会话数量限制
        let user_channels = self.user_channels.read().await;
        if let Some(conv_list) = user_channels.get(creator_id) {
            if conv_list.len() >= self.config.max_channels_per_user {
                return Err("Maximum channels per user limit reached".to_string());
            }
        }

        // 检查群聊设置
        if request.channel_type == ChannelType::Group {
            if request.name.is_none() {
                return Err("Group channel must have a name".to_string());
            }

            if !self.config.allow_public_groups && request.is_public.unwrap_or(false) {
                return Err("Public groups are not allowed".to_string());
            }

            if let Some(max_members) = request.max_members {
                if max_members > self.config.max_group_members {
                    return Err("Maximum group members limit exceeded".to_string());
                }
            }
        }

        Ok(())
    }

    /// 更新用户会话索引
    async fn update_user_channel_index(&self, channel: &Channel) {
        let mut user_channels = self.user_channels.write().await;
        
        for user_id in channel.get_member_ids() {
            user_channels.entry(user_id)
                .or_insert_with(Vec::new)
                .push(channel.id);
        }
    }

    /// 清理无效会话
    pub async fn cleanup_invalid_channels(&self) -> usize {
        let mut channels = self.channels.write().await;
        let mut user_channels = self.user_channels.write().await;
        let mut direct_index = self.direct_channel_index.write().await;

        let mut removed_count = 0;
        let mut to_remove = Vec::new();

        // 找出需要删除的会话
        for (conv_id, conv) in channels.iter() {
            if conv.status == ChannelStatus::Deleted || conv.members.is_empty() {
                to_remove.push(conv_id.clone());
            }
        }

        // 删除会话
        for conv_id in to_remove {
            if let Some(conv) = channels.remove(&conv_id) {
                removed_count += 1;

                // 从用户索引中删除
                for user_id in conv.get_member_ids() {
                    if let Some(conv_list) = user_channels.get_mut(&user_id) {
                        conv_list.retain(|id| id != &conv_id);
                    }
                }

                // 从私聊索引中删除
                if conv.channel_type == ChannelType::Direct {
                    let member_ids = conv.get_member_ids();
                    if member_ids.len() == 2 {
                        let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                        direct_index.remove(&key);
                    }
                }
            }
        }

        if removed_count > 0 {
            info!("Cleaned up {} invalid channels", removed_count);
        }

        removed_count
    }

    /// 置顶会话
    pub async fn pin_channel(&self, user_id: u64, channel_id: u64, pinned: bool) -> Result<bool> {
        let key = (user_id, channel_id);
        let mut pinned_channels = self.pinned_channels.write().await;
        
        if pinned {
            pinned_channels.insert(key);
            info!("User {} pinned channel {}", user_id, channel_id);
        } else {
            pinned_channels.remove(&key);
            info!("User {} unpinned channel {}", user_id, channel_id);
        }
        
        Ok(true)
    }

    /// 检查会话是否置顶
    pub async fn is_channel_pinned(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let pinned_channels = self.pinned_channels.read().await;
        pinned_channels.contains(&key)
    }

    /// 设置会话免打扰
    pub async fn mute_channel(&self, user_id: u64, channel_id: u64, muted: bool) -> Result<()> {
        let key = (user_id, channel_id);
        let mut muted_channels = self.muted_channels.write().await;
        
        if muted {
            muted_channels.insert(key);
            info!("User {} muted channel {}", user_id, channel_id);
        } else {
            muted_channels.remove(&key);
            info!("User {} unmuted channel {}", user_id, channel_id);
        }
        
        Ok(())
    }

    /// 检查会话是否免打扰
    pub async fn is_channel_muted(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let muted_channels = self.muted_channels.read().await;
        muted_channels.contains(&key)
    }

    /// 隐藏频道（不显示在会话列表中）
    pub async fn hide_channel(&self, user_id: u64, channel_id: u64, hidden: bool) -> Result<()> {
        // TODO: 这里应该更新数据库中的 UserChannelView.is_hidden 字段
        // 目前先使用内存存储，后续需要持久化到数据库
        let key = (user_id, channel_id);
        let mut hidden_channels = self.hidden_channels.write().await;
        
        if hidden {
            hidden_channels.insert(key);
            info!("User {} hidden channel {}", user_id, channel_id);
        } else {
            hidden_channels.remove(&key);
            info!("User {} unhidden channel {}", user_id, channel_id);
        }
        
        Ok(())
    }

    /// 检查频道是否隐藏
    pub async fn is_channel_hidden(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let hidden_channels = self.hidden_channels.read().await;
        hidden_channels.contains(&key)
    }

    /// 更新最后消息预览缓存
    pub async fn update_last_message_preview(&self, channel_id: u64, preview: LastMessagePreview) {
        let mut cache = self.last_message_cache.write().await;
        cache.insert(channel_id, preview);
    }

    /// 获取最后消息预览
    pub async fn get_last_message_preview(&self, channel_id: u64) -> Option<LastMessagePreview> {
        let cache = self.last_message_cache.read().await;
        cache.get(&channel_id).cloned()
    }

    /// 获取增强的用户会话列表（包含预览、未读、置顶等信息）
    pub async fn get_user_channels_enhanced(&self, user_id: u64) -> EnhancedChannelListResponse {
        let mut channel_ids = {
            let user_channels = self.user_channels.read().await;
            user_channels.get(&user_id).cloned().unwrap_or_default()
        };

        // 若内存中该用户无会话，从 DB 加载并填充缓存（服务重启后恢复列表）
        if channel_ids.is_empty() {
            match self.channel_repository.list_channel_ids_by_user(user_id).await {
                Ok(db_ids) => {
                    for &cid in &db_ids {
                        if let Some(channel) = self.get_channel_opt(cid).await {
                            self.update_user_channel_index(&channel).await;
                            if channel.channel_type == ChannelType::Direct {
                                let member_ids = channel.get_member_ids();
                                if member_ids.len() == 2 {
                                    let key = Self::make_direct_key(member_ids[0], member_ids[1]);
                                    let mut index = self.direct_channel_index.write().await;
                                    index.insert(key, cid);
                                }
                            }
                        }
                    }
                    let user_channels = self.user_channels.read().await;
                    channel_ids = user_channels.get(&user_id).cloned().unwrap_or_default();
                }
                Err(e) => {
                    warn!("list_channel_ids_by_user failed for user {}: {}", user_id, e);
                }
            }
        }

        let channels_lock = self.channels.read().await;
        let pinned_lock = self.pinned_channels.read().await;
        let muted_lock = self.muted_channels.read().await;
        let hidden_lock = self.hidden_channels.read().await;
        let cache_lock = self.last_message_cache.read().await;
        
        let mut items = Vec::new();
        
        for conv_id in channel_ids {
            // 过滤隐藏的频道
            if hidden_lock.contains(&(user_id, conv_id)) {
                continue;
            }
            
            if let Some(conv) = channels_lock.get(&conv_id) {
                if !conv.is_active() {
                    continue;
                }

                // 检查置顶状态
                let is_pinned = pinned_lock.contains(&(user_id, conv_id));
                
                // 检查免打扰状态
                let is_muted = muted_lock.contains(&(user_id, conv_id));
                
                // 获取最后消息预览
                let last_message = cache_lock.get(&conv_id).cloned();
                
                // 计算未读消息数（简化版：使用 last_message_id 和 user_read_position）
                let unread_count = if let Some(member) = conv.members.get(&user_id) {
                    // TODO: 实现基于消息序号的未读数计算
                    if conv.last_message_id.is_some() && member.last_read_message_id.is_none() {
                        1  // 简化处理：如果有新消息且未读，返回 1
                    } else {
                        0
                    }
                } else {
                    0
                };
                
                items.push(EnhancedChannelItem {
                    channel: conv.clone(),
                    last_message,
                    unread_count,
                    is_pinned,
                    is_muted,
                });
            }
        }
        
        drop(channels_lock);
        drop(pinned_lock);
        drop(muted_lock);
        drop(cache_lock);

        // 排序：置顶的在前，然后按最后消息时间排序
        items.sort_by(|a, b| {
            match (a.is_pinned, b.is_pinned) {
                (true, false) => std::cmp::Ordering::Less,  // a 置顶，b 不置顶，a 在前
                (false, true) => std::cmp::Ordering::Greater,  // b 置顶，a 不置顶，b 在前
                _ => {
                    // 都置顶或都不置顶，按最后消息时间排序
                    b.channel.last_message_at.cmp(&a.channel.last_message_at)
                }
            }
        });

        EnhancedChannelListResponse {
            total: items.len(),
            has_more: false,
            channels: items,
        }
    }

    // ============================================================================
    // 兼容方法：为了兼容原 channel_service 的 API
    // ============================================================================

    /// 获取或创建私聊会话（RPC channel/direct/get_or_create 使用）
    /// 返回 (channel_id, 是否本次新创建)。source/source_id 与添加好友规范一致。
    pub async fn get_or_create_direct_channel(
        &self,
        user_id: u64,
        target_user_id: u64,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<(u64, bool)> {
        let (channel, created) = self.channel_repository
            .create_or_get_direct_channel(user_id, target_user_id, source, source_id)
            .await
            .map_err(|e| ServerError::Database(format!("get_or_create_direct_channel: {}", e)))?;
        let channel_id = channel.id;
        if created {
            if let Err(e) = self.create_private_chat_with_id(user_id, target_user_id, channel_id).await {
                tracing::warn!("⚠️ 同步私聊频道到缓存失败: {}，channel_id={}", e, channel_id);
            }
        } else if self.get_channel_opt(channel_id).await.is_none() {
            // 已存在但缓存没有，从 DB 加载到缓存
            if let Err(e) = self.create_private_chat_with_id(user_id, target_user_id, channel_id).await {
                tracing::warn!("⚠️ 加载已存在私聊到缓存失败: {}，channel_id={}", e, channel_id);
            }
        }
        Ok((channel_id, created))
    }

    /// 创建私聊频道（使用指定 ID）
    pub async fn create_private_chat_with_id(
        &self,
        user1: u64,
        user2: u64,
        channel_id: u64,
    ) -> Result<()> {
        // 检查会话是否已存在
        if self.get_channel_opt(channel_id).await.is_some() {
            return Ok(()); // 已存在，直接返回成功
        }

        // 创建私聊会话
        let channel = Channel::new_direct(channel_id, user1, user2);
        
        // 保存到内存缓存
        {
            let mut channels = self.channels.write().await;
            channels.insert(channel_id, channel.clone());
        }

        // 更新用户会话索引
        self.update_user_channel_index(&channel).await;

        Ok(())
    }

    /// 创建群聊频道（使用指定 ID）
    pub async fn create_group_chat_with_id(
        &self,
        owner_id: u64,
        name: String,
        channel_id: u64,
    ) -> Result<()> {
        // 检查会话是否已存在
        if self.get_channel_opt(channel_id).await.is_some() {
            return Ok(()); // 已存在，直接返回成功
        }

        // 创建群聊会话
        let channel = Channel::new_group(channel_id, owner_id, Some(name));
        
        // 保存到内存缓存
        {
            let mut channels = self.channels.write().await;
            channels.insert(channel_id, channel.clone());
        }

        // 更新用户会话索引
        self.update_user_channel_index(&channel).await;

        Ok(())
    }

    /// 获取频道成员列表
    pub async fn get_channel_members(&self, channel_id: &u64) -> Result<Vec<crate::model::ChannelMember>> {
        if let Some(channel) = self.get_channel_opt(*channel_id).await {
            Ok(channel.members.values().cloned().collect())
        } else {
            // 如果内存中没有，从数据库查询参与者
            let participants = self.get_channel_participants(*channel_id).await
                .map_err(|e| ServerError::Database(format!("查询参与者失败: {}", e)))?;
            Ok(participants.into_iter().map(|p| p.to_channel_member()).collect())
        }
    }

    /// 添加成员到群组
    pub async fn add_member_to_group(&self, group_id: u64, user_id: u64) -> Result<()> {
        self.join_channel(group_id, user_id, None).await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("添加成员失败: {}", e)))
    }

    /// 设置成员角色
    pub async fn set_member_role(
        &self,
        channel_id: &u64,
        user_id: &u64,
        role: crate::model::channel::MemberRole,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            channel.update_member_role(user_id, role)
                .map_err(|e| ServerError::Validation(e))?;
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置成员禁言状态
    pub async fn set_member_muted(
        &self,
        channel_id: &u64,
        user_id: &u64,
        muted: bool,
        _mute_until: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(member) = channel.members.get_mut(user_id) {
                member.is_muted = muted;
                // TODO: 如果需要，可以添加 mute_until 字段到 ChannelMember
                Ok(())
            } else {
                Err(ServerError::NotFound(format!("Member not found in channel: {}", channel_id)))
            }
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 离开频道（兼容方法，接受引用参数）
    pub async fn leave_channel_ref(&self, channel_id: &u64, user_id: &u64) -> Result<()> {
        self.leave_channel(*channel_id, *user_id).await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("离开频道失败: {}", e)))
    }

    /// 加入频道（兼容方法，接受引用参数）
    pub async fn join_channel_ref(
        &self,
        channel_id: &u64,
        user_id: u64,
        role: crate::model::channel::MemberRole,
    ) -> Result<()> {
        self.join_channel(*channel_id, user_id, Some(role)).await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("加入频道失败: {}", e)))
    }

    /// 获取用户未读计数
    pub async fn get_user_unread_count(&self, user_id: &u64) -> Result<u32> {
        let user_channels = self.user_channels.read().await;
        let channel_ids = user_channels.get(user_id).cloned().unwrap_or_default();
        drop(user_channels);

        let channels = self.channels.read().await;
        let mut total_unread = 0u32;
        
        for channel_id in channel_ids {
            if let Some(channel) = channels.get(&channel_id) {
                if let Some(member) = channel.members.get(user_id) {
                    // TODO: 实现基于消息序号的未读数计算
                    if channel.last_message_id.is_some() && member.last_read_message_id.is_none() {
                        total_unread += 1;
                    }
                }
            }
        }

        Ok(total_unread)
    }

    /// 更新频道元数据
    pub async fn update_channel_metadata(
        &self,
        channel_id: &u64,
        name: Option<String>,
        description: Option<String>,
        avatar_url: Option<String>,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if let Some(name) = name {
                channel.metadata.name = Some(name);
            }
            if let Some(description) = description {
                channel.metadata.description = Some(description);
            }
            if let Some(avatar_url) = avatar_url {
                channel.metadata.avatar_url = Some(avatar_url);
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置频道公告
    pub async fn set_channel_announcement(
        &self,
        channel_id: &u64,
        announcement: Option<String>,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            channel.metadata.announcement = announcement;
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置频道全员禁言
    pub async fn set_channel_all_muted(
        &self,
        channel_id: &u64,
        muted: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.is_muted = muted;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置频道加群审批
    pub async fn set_channel_join_approval(
        &self,
        channel_id: &u64,
        require_approval: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.require_approval = require_approval;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置频道成员邀请权限
    pub async fn set_channel_member_invite(
        &self,
        channel_id: &u64,
        allow_invite: bool,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.allow_member_invite = allow_invite;
            }
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 设置频道最大成员数
    pub async fn set_channel_max_members(
        &self,
        channel_id: &u64,
        max_members: u32,
    ) -> Result<()> {
        let mut channels = self.channels.write().await;
        if let Some(channel) = channels.get_mut(channel_id) {
            if channel.settings.is_none() {
                channel.settings = Some(crate::model::channel::ChannelSettings::default());
            }
            if let Some(settings) = channel.settings.as_mut() {
                settings.max_members = Some(max_members);
            }
            channel.metadata.max_members = Some(max_members as usize);
            channel.updated_at = chrono::Utc::now();
            Ok(())
        } else {
            Err(ServerError::NotFound(format!("Channel not found: {}", channel_id)))
        }
    }

    /// 标记消息已读（兼容方法）
    pub async fn mark_as_read(
        &self,
        user_id: &u64,
        channel_id: &u64,
        message_id: u64,
    ) -> Result<()> {
        self.mark_message_read(*channel_id, *user_id, message_id).await
            .map(|_| ())
            .map_err(|e| ServerError::Internal(format!("标记已读失败: {}", e)))
    }

    /// 获取用户频道视图
    pub async fn get_user_channel(
        &self,
        user_id: &u64,
        channel_id: &u64,
    ) -> Result<crate::model::channel::UserChannelView> {
        // 先获取频道
        let channel = self.get_channel(channel_id).await?;
        
        // 检查用户是否在频道中
        if !channel.members.contains_key(user_id) {
            return Err(ServerError::NotFound(format!("User {} not in channel {}", user_id, channel_id)));
        }

        // 获取或创建 UserChannelView
        // 这里简化实现，实际应该从数据库查询 user_channel 表
        let member = channel.members.get(user_id).unwrap();
        Ok(crate::model::channel::UserChannelView {
            user_id: *user_id,
            channel_id: *channel_id,
            last_read_message_id: member.last_read_message_id,
            is_muted: member.is_muted,
            is_pinned: false, // TODO: 从数据库查询
            unread_count: 0,  // TODO: 计算未读数
            remark: None,
            custom_title: None,
            last_viewed_at: member.last_active_at,
            created_at: channel.created_at,
            updated_at: channel.updated_at,
            is_hidden: false,
            sort_weight: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::channel::ChannelType;

    #[tokio::test]
    async fn test_create_direct_channel() {
        let service = ChannelService::new_default();
        
        let request = CreateChannelRequest {
            channel_type: ChannelType::Direct,
            name: None,
            description: None,
            member_ids: vec!["user2".to_string()],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel("user1".to_string(), request).await.unwrap();
        assert!(response.success);
        assert_eq!(response.channel.channel_type, ChannelType::Direct);
        assert_eq!(response.channel.members.len(), 2);
    }

    #[tokio::test]
    async fn test_create_group_channel() {
        let service = ChannelService::new_default();
        
        let request = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("Test Group".to_string()),
            description: Some("A test group".to_string()),
            member_ids: vec!["user2".to_string(), "user3".to_string()],
            is_public: Some(false),
            max_members: Some(10),
        };

        let response = service.create_channel("user1".to_string(), request).await.unwrap();
        assert!(response.success);
        assert_eq!(response.channel.channel_type, ChannelType::Group);
        assert_eq!(response.channel.members.len(), 3); // creator + 2 members
    }

    #[tokio::test]
    async fn test_find_direct_channel() {
        let service = ChannelService::new_default();
        
        let request = CreateChannelRequest {
            channel_type: ChannelType::Direct,
            name: None,
            description: None,
            member_ids: vec!["user2".to_string()],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel("user1".to_string(), request).await.unwrap();
        assert!(response.success);

        let conv_id = response.channel.id.clone();
        let found_id = service.find_direct_channel("user1", "user2").await;
        assert_eq!(found_id, Some(conv_id.clone()));

        let found_id2 = service.find_direct_channel("user2", "user1").await;
        assert_eq!(found_id2, Some(conv_id));
    }

    #[tokio::test]
    async fn test_join_and_leave_channel() {
        let service = ChannelService::new_default();
        
        let request = CreateChannelRequest {
            channel_type: ChannelType::Group,
            name: Some("Test Group".to_string()),
            description: None,
            member_ids: vec![],
            is_public: None,
            max_members: None,
        };

        let response = service.create_channel("user1".to_string(), request).await.unwrap();
        let conv_id = response.channel.id;

        // 加入会话
        let joined = service.join_channel(&conv_id, "user2".to_string(), None).await.unwrap();
        assert!(joined);

        // 检查成员
        let conv = service.get_channel(&conv_id).await.unwrap();
        assert_eq!(conv.members.len(), 2);

        // 离开会话
        let left = service.leave_channel(&conv_id, "user2").await.unwrap();
        assert!(left);

        // 检查成员
        let conv = service.get_channel(&conv_id).await.unwrap();
        assert_eq!(conv.members.len(), 1);
    }
} 