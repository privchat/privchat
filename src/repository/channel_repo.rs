//! 会话仓库 - PostgreSQL 实现

use std::sync::Arc;
use sqlx::PgPool;
use crate::model::Channel;
use crate::error::DatabaseError;

/// 会话仓库 trait
pub trait ChannelRepository: Send + Sync {
    /// 根据ID查找会话
    async fn find_by_id(&self, channel_id: u64) -> Result<Option<Channel>, DatabaseError>;
    
    /// 创建会话（通用方法）
    async fn create(&self, channel: &Channel) -> Result<Channel, DatabaseError>;
    
    /// 创建或获取私聊会话。返回 (会话, 是否本次新创建)。
    /// source/source_id 与添加好友规范一致，仅在新创建时写入 DB。
    async fn create_or_get_direct_channel(
        &self,
        user1_id: u64,
        user2_id: u64,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<(Channel, bool), DatabaseError>;
    
    /// 创建群聊会话
    async fn create_group_channel(&self, group_id: u64) -> Result<Channel, DatabaseError>;
    
    /// 更新会话
    async fn update(&self, channel: &Channel) -> Result<Channel, DatabaseError>;
    
    /// 删除会话
    async fn delete(&self, channel_id: u64) -> Result<(), DatabaseError>;
    
    /// 获取会话参与者
    async fn get_participants(&self, channel_id: u64) -> Result<Vec<crate::model::ChannelParticipant>, DatabaseError>;
    
    /// 按用户从 DB 查询其参与且未退出的会话 ID 列表（用于服务重启后恢复会话列表）
    async fn list_channel_ids_by_user(&self, user_id: u64) -> Result<Vec<u64>, DatabaseError>;
    
    /// 添加参与者到数据库
    async fn add_participant(&self, channel_id: u64, user_id: u64, role: crate::model::channel::MemberRole) -> Result<(), DatabaseError>;
}

/// 会话仓库 (PostgreSQL 实现)
#[derive(Clone)]
pub struct PgChannelRepository {
    pool: Arc<PgPool>,
}

impl PgChannelRepository {
    /// 创建新的会话仓库
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }
    
    /// 获取数据库连接池（用于直接执行 SQL）
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

impl ChannelRepository for PgChannelRepository {
    /// 根据ID查找会话
    async fn find_by_id(&self, channel_id: u64) -> Result<Option<Channel>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct ChannelRow {
            channel_id: i64,  // PostgreSQL BIGINT
            channel_type: i16,
            direct_user1_id: Option<i64>,  // PostgreSQL BIGINT
            direct_user2_id: Option<i64>,  // PostgreSQL BIGINT
            group_id: Option<i64>,  // PostgreSQL BIGINT
            last_message_id: Option<i64>,  // PostgreSQL BIGINT
            last_message_at: Option<i64>,
            message_count: i64,
            created_at: i64,
            updated_at: i64,
        }
        
        let row = sqlx::query_as::<_, ChannelRow>(
            r#"
            SELECT 
                channel_id,
                channel_type,
                direct_user1_id,
                direct_user2_id,
                group_id,
                last_message_id,
                last_message_at,
                message_count,
                created_at,
                updated_at
            FROM privchat_channels
            WHERE channel_id = $1
            "#
        )
        .bind(channel_id as i64)  // PostgreSQL BIGINT
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query channel: {}", e)))?;
        
        match row {
            Some(r) => {
                Ok(Some(Channel::from_db_row(
                    r.channel_id,
                    r.channel_type,
                    r.direct_user1_id,
                    r.direct_user2_id,
                    r.group_id,
                    r.last_message_id,
                    r.last_message_at,
                    r.message_count,
                    r.created_at,
                    r.updated_at,
                )))
            }
            None => Ok(None),
        }
    }
    
    /// 创建会话（通用方法）
    async fn create(&self, channel: &Channel) -> Result<Channel, DatabaseError> {
        let (channel_id, channel_type, direct_user1_id, direct_user2_id, group_id, last_message_id, last_message_at, message_count, created_at, updated_at) = channel.to_db_values();
        
        tracing::info!("🔧 [ChannelRepo] create() 开始: channel_id={}, type={}, user1={:?}, user2={:?}", 
                      channel_id, channel_type, direct_user1_id, direct_user2_id);
        
        // 如果 channel_id 为 0，说明需要数据库自动生成（BIGSERIAL）
        let actual_conv_id = if channel_id == 0 {
            tracing::info!("🔧 [ChannelRepo] 使用数据库自增ID");
            let row = sqlx::query_as::<_, (i64,)>(
                r#"
                INSERT INTO privchat_channels (
                    channel_type, direct_user1_id, direct_user2_id,
                    group_id, last_message_id, last_message_at, message_count,
                    created_at, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING channel_id
                "#
            )
            .bind(channel_type)
            .bind(direct_user1_id)
            .bind(direct_user2_id)
            .bind(group_id)
            .bind(last_message_id)
            .bind(last_message_at)
            .bind(message_count)
            .bind(created_at)
            .bind(updated_at)
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| {
                tracing::error!("❌ [ChannelRepo] 数据库插入失败: {}", e);
                DatabaseError::Database(format!("Failed to create channel: {}", e))
            })?;
            
            let id = row.0 as u64;
            tracing::info!("✅ [ChannelRepo] 数据库插入成功，返回ID: {}", id);
            id
        } else {
            tracing::info!("🔧 [ChannelRepo] 使用指定ID: {}", channel_id);
            // 如果指定了 channel_id，使用指定的值
            sqlx::query(
                r#"
                INSERT INTO privchat_channels (
                    channel_id, channel_type, direct_user1_id, direct_user2_id,
                    group_id, last_message_id, last_message_at, message_count,
                    created_at, updated_at
                )
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (channel_id) DO UPDATE SET
                    channel_type = EXCLUDED.channel_type,
                    direct_user1_id = EXCLUDED.direct_user1_id,
                    direct_user2_id = EXCLUDED.direct_user2_id,
                    group_id = EXCLUDED.group_id,
                    last_message_id = EXCLUDED.last_message_id,
                    last_message_at = EXCLUDED.last_message_at,
                    message_count = EXCLUDED.message_count,
                    updated_at = EXCLUDED.updated_at
                "#
            )
            .bind(channel_id as i64)
            .bind(channel_type)
            .bind(direct_user1_id)
            .bind(direct_user2_id)
            .bind(group_id)
            .bind(last_message_id)
            .bind(last_message_at)
            .bind(message_count)
            .bind(created_at)
            .bind(updated_at)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to create channel: {}", e)))?;
            
            channel_id as u64
        };
        
        // 返回创建的会话
        self.find_by_id(actual_conv_id).await
            .and_then(|opt| opt.ok_or_else(|| DatabaseError::NotFound("Channel not found after creation".to_string())))
    }
    
    /// 创建或获取私聊会话
    async fn create_or_get_direct_channel(
        &self,
        user1_id: u64,
        user2_id: u64,
        source: Option<&str>,
        source_id: Option<&str>,
    ) -> Result<(Channel, bool), DatabaseError> {
        // 先尝试查找已存在的会话（两个方向都要检查）
        let existing = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT channel_id
            FROM privchat_channels
            WHERE channel_type = 0
            AND (
                (direct_user1_id = $1 AND direct_user2_id = $2)
                OR (direct_user1_id = $2 AND direct_user2_id = $1)
            )
            LIMIT 1
            "#
        )
        .bind(user1_id as i64)
        .bind(user2_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query existing channel: {}", e)))?;
        
        if let Some((conv_id,)) = existing {
            // 返回已存在的会话（非本次创建）
            let ch = self.find_by_id(conv_id as u64).await
                .and_then(|opt| opt.ok_or_else(|| DatabaseError::NotFound("Channel not found".to_string())))?;
            return Ok((ch, false));
        }
        
        // 创建新会话 - channel_id 由数据库自动生成（BIGSERIAL），写入来源
        let now = chrono::Utc::now().timestamp_millis();
        
        let conv_id = sqlx::query_as::<_, (i64,)>(
            r#"
            INSERT INTO privchat_channels (
                channel_type, direct_user1_id, direct_user2_id,
                create_source, create_source_id,
                created_at, updated_at
            )
            VALUES (0, $1, $2, $3, $4, $5, $5)
            RETURNING channel_id
            "#
        )
        .bind(user1_id as i64)
        .bind(user2_id as i64)
        .bind(source)
        .bind(source_id)
        .bind(now)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create channel: {}", e)))?;
        
        let conv_id_u64 = conv_id.0 as u64;
        
        // 创建参与者记录
        sqlx::query(
            r#"
            INSERT INTO privchat_channel_participants (
                channel_id, user_id, role, joined_at
            )
            VALUES ($1, $2, 2, $3), ($1, $4, 2, $3)
            ON CONFLICT DO NOTHING
            "#
        )
        .bind(conv_id_u64 as i64)
        .bind(user1_id as i64)
        .bind(now)
        .bind(user2_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create participants: {}", e)))?;
        
        let ch = self.find_by_id(conv_id_u64).await
            .and_then(|opt| opt.ok_or_else(|| DatabaseError::NotFound("Channel not found after creation".to_string())))?;
        Ok((ch, true))
    }
    
    /// 创建群聊会话
    async fn create_group_channel(&self, group_id: u64) -> Result<Channel, DatabaseError> {
        // 检查是否已存在
        let existing = sqlx::query_as::<_, (i64,)>(
            "SELECT channel_id FROM privchat_channels WHERE group_id = $1 AND channel_type = 1 LIMIT 1"
        )
        .bind(group_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query existing group channel: {}", e)))?;
        
        if let Some((conv_id,)) = existing {
            return self.find_by_id(conv_id as u64).await
                .and_then(|opt| opt.ok_or_else(|| DatabaseError::NotFound("Channel not found".to_string())));
        }
        
        // 创建新会话 - channel_id 由数据库自动生成（BIGSERIAL）
        let now = chrono::Utc::now().timestamp_millis();
        
        let conv_id = sqlx::query_as::<_, (i64,)>(
            r#"
            INSERT INTO privchat_channels (
                channel_type, group_id,
                created_at, updated_at
            )
            VALUES (1, $1, $2, $2)
            RETURNING channel_id
            "#
        )
        .bind(group_id as i64)
        .bind(now)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create group channel: {}", e)))?;
        
        // 返回创建的会话
        self.find_by_id(conv_id.0 as u64).await
            .and_then(|opt| opt.ok_or_else(|| DatabaseError::NotFound("Channel not found after creation".to_string())))
    }
    
    /// 更新会话
    async fn update(&self, channel: &Channel) -> Result<Channel, DatabaseError> {
        let (conv_id, conv_type, direct_user1_id, direct_user2_id, group_id, last_message_id, last_message_at, message_count, _created_at, updated_at) = channel.to_db_values();
        
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_channels
            SET 
                channel_type = $2,
                direct_user1_id = $3,
                direct_user2_id = $4,
                group_id = $5,
                last_message_id = $6,
                last_message_at = $7,
                message_count = $8,
                updated_at = $9
            WHERE channel_id = $1
            "#
        )
        .bind(conv_id)
        .bind(conv_type)
        .bind(direct_user1_id)
        .bind(direct_user2_id)
        .bind(group_id)
        .bind(last_message_id)
        .bind(last_message_at)
        .bind(message_count)
        .bind(updated_at)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to update channel: {}", e)))?;
        
        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Channel not found".to_string()));
        }
        
        Ok(channel.clone())
    }
    
    /// 删除会话
    async fn delete(&self, channel_id: u64) -> Result<(), DatabaseError> {
        let rows_affected = sqlx::query(
            "DELETE FROM privchat_channels WHERE channel_id = $1"
        )
        .bind(channel_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to delete channel: {}", e)))?;
        
        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Channel not found".to_string()));
        }
        
        Ok(())
    }
    
    /// 获取会话参与者
    async fn get_participants(&self, channel_id: u64) -> Result<Vec<crate::model::ChannelParticipant>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct ParticipantRow {
            channel_id: i64,  // PostgreSQL BIGINT
            user_id: i64,  // PostgreSQL BIGINT
            role: i16,
            nickname: Option<String>,
            permissions: Option<serde_json::Value>,
            mute_until: Option<i64>,
            joined_at: i64,
            left_at: Option<i64>,
        }
        
        let rows = sqlx::query_as::<_, ParticipantRow>(
            r#"
            SELECT 
                channel_id,
                user_id,
                role,
                nickname,
                permissions,
                mute_until,
                joined_at,
                left_at
            FROM privchat_channel_participants
            WHERE channel_id = $1 AND left_at IS NULL
            ORDER BY joined_at ASC
            "#
        )
        .bind(channel_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query participants: {}", e)))?;
        
        Ok(rows.into_iter().map(|r| {
            crate::model::ChannelParticipant::from_db_row(
                r.channel_id,
                r.user_id,
                r.role,
                r.nickname,
                r.permissions.unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
                r.mute_until,
                r.joined_at,
                r.left_at,
            )
        }).collect())
    }
    
    /// 按用户从 DB 查询其参与且未退出的会话 ID 列表
    async fn list_channel_ids_by_user(&self, user_id: u64) -> Result<Vec<u64>, DatabaseError> {
        let rows = sqlx::query_as::<_, (i64,)>(
            r#"
            SELECT DISTINCT channel_id
            FROM privchat_channel_participants
            WHERE user_id = $1 AND left_at IS NULL
            ORDER BY channel_id ASC
            "#
        )
        .bind(user_id as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to list channel ids by user: {}", e)))?;
        Ok(rows.into_iter().map(|(id,)| id as u64).collect())
    }
    
    /// 添加参与者到数据库
    async fn add_participant(&self, channel_id: u64, user_id: u64, role: crate::model::channel::MemberRole) -> Result<(), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();
        
        sqlx::query(
            r#"
            INSERT INTO privchat_channel_participants (
                channel_id, user_id, role, joined_at
            )
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (channel_id, user_id) DO UPDATE SET
                role = EXCLUDED.role,
                left_at = NULL
            "#
        )
        .bind(channel_id as i64)
        .bind(user_id as i64)
        .bind(role as i16)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to add participant: {}", e)))?;
        
        Ok(())
    }
}
