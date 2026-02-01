//! 消息仓库 - PostgreSQL 实现

use std::sync::Arc;
use sqlx::PgPool;
use uuid::Uuid;
use crate::model::message::Message;
use crate::error::DatabaseError;

/// 消息仓库 trait
pub trait MessageRepository: Send + Sync {
    /// 根据ID查找消息
    async fn find_by_id(&self, message_id: u64) -> Result<Option<Message>, DatabaseError>;
    
    /// 创建消息
    async fn create(&self, message: &Message) -> Result<Message, DatabaseError>;
    
    /// 更新消息
    async fn update(&self, message: &Message) -> Result<Message, DatabaseError>;
    
    /// 删除消息（软删除）
    async fn delete(&self, message_id: &Uuid) -> Result<(), DatabaseError>;
    
    /// 获取会话的消息列表（分页）
    async fn list_by_channel(
        &self,
        channel_id: u64,
        limit: i64,
        before_created_at: Option<i64>,
    ) -> Result<Vec<Message>, DatabaseError>;
    
    /// 根据 pts 获取消息
    async fn find_by_pts(&self, sender_id: &Uuid, pts: i64) -> Result<Option<Message>, DatabaseError>;
    
    /// 撤回消息（标记为已撤回，但保留内容）
    async fn revoke_message(&self, message_id: u64, revoker_id: u64) -> Result<Message, DatabaseError>;
}

/// 消息仓库 (PostgreSQL 实现)
#[derive(Clone)]
pub struct PgMessageRepository {
    pool: Arc<PgPool>,
}

impl PgMessageRepository {
    /// 创建新的消息仓库
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }
    
    /// 获取数据库连接池（用于管理 API）
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
    
    // =====================================================
    // 管理 API 方法
    // =====================================================
    
    /// 获取消息列表（管理 API）
    pub async fn list_messages_admin(
        &self,
        channel_id: Option<u64>,
        user_id: Option<u64>,
        start_time: Option<i64>,
        end_time: Option<i64>,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<serde_json::Value>, u32), DatabaseError> {
        let offset = (page - 1) * page_size;
        
        let mut sql = String::from(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE 1=1
            "#
        );
        
        let mut bind_count = 0;
        if channel_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }
        
        sql.push_str(" ORDER BY created_at DESC");
        bind_count += 1;
        sql.push_str(&format!(" LIMIT ${}", bind_count));
        bind_count += 1;
        sql.push_str(&format!(" OFFSET ${}", bind_count));
        
        let mut query_builder = sqlx::query_as::<_, MessageRow>(&sql);
        
        if let Some(ch_id) = channel_id {
            query_builder = query_builder.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            query_builder = query_builder.bind(uid as i64);
        }
        if let Some(st) = start_time {
            query_builder = query_builder.bind(st);
        }
        if let Some(et) = end_time {
            query_builder = query_builder.bind(et);
        }
        query_builder = query_builder.bind(page_size as i64);
        query_builder = query_builder.bind(offset as i64);
        
        let rows = query_builder
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("查询消息列表失败: {}", e)))?;
        
        // 统计总数
        let mut count_sql = String::from("SELECT COUNT(*) FROM privchat_messages WHERE 1=1");
        bind_count = 0;
        if channel_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND channel_id = ${}", bind_count));
        }
        if user_id.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND sender_id = ${}", bind_count));
        }
        if start_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at >= ${}", bind_count));
        }
        if end_time.is_some() {
            bind_count += 1;
            count_sql.push_str(&format!(" AND created_at <= ${}", bind_count));
        }
        
        let mut count_query = sqlx::query_as::<_, (i64,)>(&count_sql);
        if let Some(ch_id) = channel_id {
            count_query = count_query.bind(ch_id as i64);
        }
        if let Some(uid) = user_id {
            count_query = count_query.bind(uid as i64);
        }
        if let Some(st) = start_time {
            count_query = count_query.bind(st);
        }
        if let Some(et) = end_time {
            count_query = count_query.bind(et);
        }
        
        let total_result: (i64,) = count_query
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("统计消息数失败: {}", e)))?;
        
        let total = total_result.0 as u32;
        
        let message_list: Vec<serde_json::Value> = rows.into_iter().map(|r| {
            serde_json::json!({
                "message_id": r.message_id as u64,
                "channel_id": r.channel_id as u64,
                "sender_id": r.sender_id as u64,
                "pts": r.pts,
                "local_message_id": r.local_message_id.map(|n| n as u64),
                "content": r.content,
                "message_type": r.message_type,
                "metadata": r.metadata,
                "reply_to_message_id": r.reply_to_message_id.map(|id| id as u64),
                "created_at": r.created_at,
                "updated_at": r.updated_at,
                "deleted": r.deleted,
                "deleted_at": r.deleted_at,
                "revoked": r.revoked,
                "revoked_at": r.revoked_at,
                "revoked_by": r.revoked_by.map(|id| id as u64),
            })
        }).collect();
        
        Ok((message_list, total))
    }
    
    /// 获取消息详情（管理 API）
    pub async fn get_message_admin(&self, message_id: u64) -> Result<Option<serde_json::Value>, DatabaseError> {
        let message = self.find_by_id(message_id).await?;
        
        Ok(message.map(|m| serde_json::json!({
            "message_id": m.message_id,
            "channel_id": m.channel_id,
            "sender_id": m.sender_id,
            "pts": m.pts,
            "local_message_id": m.local_message_id,
            "content": m.content,
            "message_type": m.message_type as u32,
            "metadata": m.metadata,
            "reply_to_message_id": m.reply_to_message_id,
            "created_at": m.created_at.timestamp_millis(),
            "updated_at": m.updated_at.timestamp_millis(),
            "deleted": m.deleted,
            "deleted_at": m.deleted_at.map(|dt| dt.timestamp_millis()),
            "revoked": m.revoked,
            "revoked_at": m.revoked_at.map(|dt| dt.timestamp_millis()),
            "revoked_by": m.revoked_by,
        })))
    }
    
    /// 获取消息统计（管理 API）
    pub async fn get_message_stats_admin(&self) -> Result<serde_json::Value, DatabaseError> {
        let now = chrono::Utc::now();
        let today_start = now.date_naive().and_hms_opt(0, 0, 0).unwrap();
        let today_start_ts: i64 = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(today_start, chrono::Utc).timestamp_millis();
        
        let week_start = today_start - chrono::Duration::days(7);
        let week_start_ts: i64 = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(week_start, chrono::Utc).timestamp_millis();
        
        let month_start = today_start - chrono::Duration::days(30);
        let month_start_ts: i64 = chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(month_start, chrono::Utc).timestamp_millis();
        
        let total_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages"
        )
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计总消息数失败: {}", e)))?;
        
        let today_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1"
        )
        .bind(today_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计今日消息数失败: {}", e)))?;
        
        let week_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1"
        )
        .bind(week_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计本周消息数失败: {}", e)))?;
        
        let month_result = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM privchat_messages WHERE created_at >= $1"
        )
        .bind(month_start_ts)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("统计本月消息数失败: {}", e)))?;
        
        Ok(serde_json::json!({
            "total": total_result.0 as u64,
            "today": today_result.0 as u64,
            "this_week": week_result.0 as u64,
            "this_month": month_result.0 as u64,
        }))
    }
}

#[derive(sqlx::FromRow)]
struct MessageRow {
    message_id: i64,
    channel_id: i64,
    sender_id: i64,
    pts: i64,
    local_message_id: Option<i64>,
    content: String,
    message_type: i16,
    metadata: Option<serde_json::Value>,
    reply_to_message_id: Option<i64>,
    created_at: i64,
    updated_at: i64,
    deleted: bool,
    deleted_at: Option<i64>,
    revoked: bool,
    revoked_at: Option<i64>,
    revoked_by: Option<i64>,
}

impl MessageRepository for PgMessageRepository {
    /// 根据ID查找消息
    async fn find_by_id(&self, message_id: u64) -> Result<Option<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }
        
        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE message_id = $1
            ORDER BY created_at DESC
            LIMIT 1
            "#
        )
        .bind(message_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query message: {}", e)))?;
        
        match row {
            Some(r) => {
                Ok(Some(Message::from_db_row(
                    r.message_id,
                    r.channel_id,
                    r.sender_id,
                    r.pts,
                    r.local_message_id.map(|n| n as u64),
                    r.content,
                    r.message_type,
                    r.metadata.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.reply_to_message_id,
                    r.created_at,
                    r.updated_at,
                    r.deleted,
                    r.deleted_at,
                    r.revoked,
                    r.revoked_at,
                    r.revoked_by,
                )))
            }
            None => Ok(None),
        }
    }
    
    /// 创建消息
    async fn create(&self, message: &Message) -> Result<Message, DatabaseError> {
        let (message_id, channel_id, sender_id, pts, local_message_id, content, message_type, metadata, reply_to_message_id, created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by) = message.to_db_values();
        
        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        
        sqlx::query(
            r#"
            INSERT INTO privchat_messages (
                message_id, channel_id, sender_id, pts, local_message_id,
                message_type, content, metadata, reply_to_message_id,
                created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
            "#
        )
        .bind(message_id)
        .bind(channel_id)
        .bind(sender_id)
        .bind(pts)
        .bind(local_message_id)
        .bind(message_type)
        .bind(content)
        .bind(&metadata_json)
        .bind(reply_to_message_id)
        .bind(created_at)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create message: {}", e)))?;
        
        Ok(message.clone())
    }
    
    /// 更新消息
    async fn update(&self, message: &Message) -> Result<Message, DatabaseError> {
        let (message_id, _channel_id, _sender_id, _pts, _local_message_id, content, _message_type, metadata, _reply_to_message_id, _created_at, updated_at, deleted, deleted_at, revoked, revoked_at, revoked_by) = message.to_db_values();
        
        let metadata_json = serde_json::to_value(&metadata)
            .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
        
        // 注意：由于消息表是分区表，需要包含 created_at 在 WHERE 条件中
        // 但更新时我们只更新非分区键字段
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET 
                content = $2,
                metadata = $3,
                updated_at = $4,
                deleted = $5,
                deleted_at = $6,
                revoked = $7,
                revoked_at = $8,
                revoked_by = $9
            WHERE message_id = $1
            "#
        )
        .bind(message_id)
        .bind(content)
        .bind(&metadata_json)
        .bind(updated_at)
        .bind(deleted)
        .bind(deleted_at)
        .bind(revoked)
        .bind(revoked_at)
        .bind(revoked_by)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to update message: {}", e)))?;
        
        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Message not found".to_string()));
        }
        
        Ok(message.clone())
    }
    
    /// 删除消息（软删除）
    async fn delete(&self, message_id: &Uuid) -> Result<(), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();
        
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET deleted = true, deleted_at = $2, updated_at = $2
            WHERE message_id = $1
            "#
        )
        .bind(message_id)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to delete message: {}", e)))?;
        
        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("Message not found".to_string()));
        }
        
        Ok(())
    }
    
    /// 获取会话的消息列表（分页）
    async fn list_by_channel(
        &self,
        channel_id: u64,
        limit: i64,
        before_created_at: Option<i64>,
    ) -> Result<Vec<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }
        
        let query = if let Some(before_ts) = before_created_at {
            sqlx::query_as::<_, MessageRow>(
                r#"
                SELECT 
                    message_id,
                    channel_id,
                    sender_id,
                    pts,
                    local_message_id,
                    content,
                    message_type,
                    metadata,
                    reply_to_message_id,
                    created_at,
                    updated_at,
                    deleted,
                    deleted_at,
                    revoked,
                    revoked_at,
                    revoked_by
                FROM privchat_messages
                WHERE channel_id = $1 AND created_at < $2 AND deleted = false
                ORDER BY created_at DESC
                LIMIT $3
                "#
            )
            .bind(channel_id as i64)
            .bind(before_ts)
            .bind(limit)
        } else {
            sqlx::query_as::<_, MessageRow>(
                r#"
                SELECT 
                    message_id,
                    channel_id,
                    sender_id,
                    pts,
                    local_message_id,
                    content,
                    message_type,
                    metadata,
                    reply_to_message_id,
                    created_at,
                    updated_at,
                    deleted,
                    deleted_at,
                    revoked,
                    revoked_at,
                    revoked_by
                FROM privchat_messages
                WHERE channel_id = $1 AND deleted = false
                ORDER BY created_at DESC
                LIMIT $2
                "#
            )
            .bind(channel_id as i64)
            .bind(limit)
        };
        
        let rows = query
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to list messages: {}", e)))?;
        
        Ok(rows.into_iter().map(|r| {
            Message::from_db_row(
                r.message_id,
                r.channel_id,
                r.sender_id,
                r.pts,
                r.local_message_id.map(|n| n as u64),
                r.content,
                r.message_type,
                r.metadata.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.reply_to_message_id,
                r.created_at,
                r.updated_at,
                r.deleted,
                r.deleted_at,
                r.revoked,
                r.revoked_at,
                r.revoked_by,
            )
        }).collect())
    }
    
    /// 根据 pts 获取消息
    async fn find_by_pts(&self, sender_id: &Uuid, pts: i64) -> Result<Option<Message>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct MessageRow {
            message_id: i64,
            channel_id: i64,
            sender_id: i64,
            pts: i64,
            local_message_id: Option<i64>,
            content: String,
            message_type: i16,
            metadata: Option<serde_json::Value>,
            reply_to_message_id: Option<i64>,
            created_at: i64,
            updated_at: i64,
            deleted: bool,
            deleted_at: Option<i64>,
            revoked: bool,
            revoked_at: Option<i64>,
            revoked_by: Option<i64>,
        }
        
        let row = sqlx::query_as::<_, MessageRow>(
            r#"
            SELECT 
                message_id,
                channel_id,
                sender_id,
                pts,
                local_message_id,
                content,
                message_type,
                metadata,
                reply_to_message_id,
                created_at,
                updated_at,
                deleted,
                deleted_at,
                revoked,
                revoked_at,
                revoked_by
            FROM privchat_messages
            WHERE sender_id = $1 AND pts = $2
            LIMIT 1
            "#
        )
        .bind(sender_id)
        .bind(pts)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query message by pts: {}", e)))?;
        
        match row {
            Some(r) => {
                Ok(Some(Message::from_db_row(
                    r.message_id,
                    r.channel_id,
                    r.sender_id,
                    r.pts,
                    r.local_message_id.map(|n| n as u64),
                    r.content,
                    r.message_type,
                    r.metadata.unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.reply_to_message_id,
                    r.created_at,
                    r.updated_at,
                    r.deleted,
                    r.deleted_at,
                    r.revoked,
                    r.revoked_at,
                    r.revoked_by,
                )))
            }
            None => Ok(None),
        }
    }
    
    /// 撤回消息（标记为已撤回，但保留内容）
    async fn revoke_message(&self, message_id: u64, revoker_id: u64) -> Result<Message, DatabaseError> {
        let revoked_at = chrono::Utc::now().timestamp_millis();
        
        // 先查询消息是否存在
        let message = self.find_by_id(message_id).await?
            .ok_or_else(|| DatabaseError::NotFound(format!("Message {} not found", message_id)))?;
        
        // 检查是否已经撤回
        if message.revoked {
            return Err(DatabaseError::Validation("Message already revoked".to_string()));
        }
        
        // 更新消息状态（只更新 revoked 相关字段，保留 content）
        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_messages
            SET revoked = true,
                revoked_at = $1,
                revoked_by = $2,
                updated_at = $1
            WHERE message_id = $3
            "#
        )
        .bind(revoked_at)
        .bind(revoker_id as i64)
        .bind(message_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to revoke message: {}", e)))?;
        
        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound(format!("Message {} not found", message_id)));
        }
        
        // 返回更新后的消息
        let mut revoked_message = message;
        revoked_message.revoked = true;
        revoked_message.revoked_at = Some(chrono::DateTime::from_timestamp_millis(revoked_at).unwrap());
        revoked_message.revoked_by = Some(revoker_id);
        revoked_message.updated_at = chrono::DateTime::from_timestamp_millis(revoked_at).unwrap();
        
        Ok(revoked_message)
    }
}
