//! 用户仓库 - PostgreSQL 实现

use crate::error::DatabaseError;
use crate::model::user::User;
use sqlx::PgPool;
use std::sync::Arc;

/// 用户仓库 (PostgreSQL 实现)
#[derive(Clone)]
pub struct UserRepository {
    pool: Arc<PgPool>,
}

impl UserRepository {
    /// 创建新的用户仓库
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 根据ID查找用户
    pub async fn find_by_id(&self, user_id: u64) -> Result<Option<User>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            WHERE user_id = $1
            "#,
        )
        .bind(user_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query user: {}", e)))?;

        match row {
            Some(r) => Ok(Some(User::from_db_row(
                r.user_id,
                r.username,
                r.password_hash,
                r.phone,
                r.email,
                r.display_name,
                r.avatar_url,
                r.user_type.unwrap_or(0),
                r.status.unwrap_or(0),
                r.privacy_settings
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.created_at,
                r.updated_at,
                r.last_active_at,
            ))),
            None => Ok(None),
        }
    }

    /// 根据用户名查找用户
    pub async fn find_by_username(&self, username: &str) -> Result<Option<User>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            WHERE LOWER(username) = LOWER($1)
            "#,
        )
        .bind(username)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query user by username: {}", e)))?;

        match row {
            Some(r) => Ok(Some(User::from_db_row(
                r.user_id,
                r.username,
                r.password_hash,
                r.phone,
                r.email,
                r.display_name,
                r.avatar_url,
                r.user_type.unwrap_or(0),
                r.status.unwrap_or(0),
                r.privacy_settings
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.created_at,
                r.updated_at,
                r.last_active_at,
            ))),
            None => Ok(None),
        }
    }

    /// 根据邮箱查找用户
    pub async fn find_by_email(&self, email: &str) -> Result<Option<User>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let row = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            WHERE LOWER(email) = LOWER($1)
            "#,
        )
        .bind(email)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query user by email: {}", e)))?;

        match row {
            Some(r) => Ok(Some(User::from_db_row(
                r.user_id,
                r.username,
                r.password_hash,
                r.phone,
                r.email,
                r.display_name,
                r.avatar_url,
                r.user_type.unwrap_or(0),
                r.status.unwrap_or(0),
                r.privacy_settings
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                r.created_at,
                r.updated_at,
                r.last_active_at,
            ))),
            None => Ok(None),
        }
    }

    /// 创建用户
    pub async fn create(&self, user: &User) -> Result<User, DatabaseError> {
        let (
            _user_id,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            privacy_settings,
            created_at,
            updated_at,
            last_active_at,
        ) = user.to_db_values();

        // 检查用户名是否已存在
        let exists: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM privchat_users WHERE LOWER(username) = LOWER($1)")
                .bind(&username)
                .fetch_optional(self.pool.as_ref())
                .await
                .map_err(|e| DatabaseError::Database(format!("Failed to check username: {}", e)))?;

        if exists.is_some() {
            return Err(DatabaseError::DuplicateEntry(
                "Username already exists".to_string(),
            ));
        }

        // 检查邮箱是否已存在
        if let Some(ref email_val) = email {
            let exists: Option<(i32,)> =
                sqlx::query_as("SELECT 1 FROM privchat_users WHERE LOWER(email) = LOWER($1)")
                    .bind(email_val)
                    .fetch_optional(self.pool.as_ref())
                    .await
                    .map_err(|e| {
                        DatabaseError::Database(format!("Failed to check email: {}", e))
                    })?;

            if exists.is_some() {
                return Err(DatabaseError::DuplicateEntry(
                    "Email already exists".to_string(),
                ));
            }
        }

        let result = sqlx::query!(
            r#"
            INSERT INTO privchat_users (
                username, password_hash, phone, email, display_name, avatar_url,
                user_type, status, privacy_settings, created_at, updated_at, last_active_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            RETURNING user_id
            "#,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            serde_json::to_value(&privacy_settings)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            created_at,
            updated_at,
            last_active_at
        )
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create user: {}", e)))?;

        // 返回创建的用户（包含数据库生成的 user_id）
        let mut created_user = user.clone();
        created_user.id = result.user_id as u64;
        Ok(created_user)
    }

    /// 创建用户（指定 user_id，用于系统用户等特殊情况）
    pub async fn create_with_id(&self, user: &User) -> Result<User, DatabaseError> {
        let (
            user_id,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            privacy_settings,
            created_at,
            updated_at,
            last_active_at,
        ) = user.to_db_values();

        // 检查 user_id 是否已存在
        let exists: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM privchat_users WHERE user_id = $1")
                .bind(user_id as i64)
                .fetch_optional(self.pool.as_ref())
                .await
                .map_err(|e| DatabaseError::Database(format!("Failed to check user_id: {}", e)))?;

        if exists.is_some() {
            return Err(DatabaseError::DuplicateEntry(format!(
                "User with id {} already exists",
                user_id
            )));
        }

        // 检查用户名是否已存在
        let exists: Option<(i32,)> =
            sqlx::query_as("SELECT 1 FROM privchat_users WHERE LOWER(username) = LOWER($1)")
                .bind(&username)
                .fetch_optional(self.pool.as_ref())
                .await
                .map_err(|e| DatabaseError::Database(format!("Failed to check username: {}", e)))?;

        if exists.is_some() {
            return Err(DatabaseError::DuplicateEntry(
                "Username already exists".to_string(),
            ));
        }

        // 检查邮箱是否已存在
        if let Some(ref email_val) = email {
            let exists: Option<(i32,)> =
                sqlx::query_as("SELECT 1 FROM privchat_users WHERE LOWER(email) = LOWER($1)")
                    .bind(email_val)
                    .fetch_optional(self.pool.as_ref())
                    .await
                    .map_err(|e| {
                        DatabaseError::Database(format!("Failed to check email: {}", e))
                    })?;

            if exists.is_some() {
                return Err(DatabaseError::DuplicateEntry(
                    "Email already exists".to_string(),
                ));
            }
        }

        let result = sqlx::query!(
            r#"
            INSERT INTO privchat_users (
                user_id, username, password_hash, phone, email, display_name, avatar_url,
                user_type, status, privacy_settings, created_at, updated_at, last_active_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            RETURNING user_id
            "#,
            user_id as i64,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            serde_json::to_value(&privacy_settings)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            created_at,
            updated_at,
            last_active_at
        )
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to create user with id: {}", e)))?;

        // 返回创建的用户
        let mut created_user = user.clone();
        created_user.id = result.user_id as u64;
        Ok(created_user)
    }

    /// 更新用户
    pub async fn update(&self, user: &User) -> Result<User, DatabaseError> {
        let (
            user_id,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            privacy_settings,
            _created_at,
            updated_at,
            last_active_at,
        ) = user.to_db_values();

        let rows_affected = sqlx::query!(
            r#"
            UPDATE privchat_users
            SET 
                username = $2,
                password_hash = $3,
                phone = $4,
                email = $5,
                display_name = $6,
                avatar_url = $7,
                user_type = $8,
                status = $9,
                privacy_settings = $10,
                updated_at = $11,
                last_active_at = $12
            WHERE user_id = $1
            "#,
            user_id,
            username,
            password_hash,
            phone,
            email,
            display_name,
            avatar_url,
            user_type,
            status,
            serde_json::to_value(&privacy_settings)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
            updated_at,
            last_active_at
        )
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to update user: {}", e)))?;

        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("User not found".to_string()));
        }

        Ok(user.clone())
    }

    /// 更新用户最后登录时间
    pub async fn update_last_login(
        &self,
        user_id: u64,
        _timestamp: u64,
    ) -> Result<(), DatabaseError> {
        let now = chrono::Utc::now().timestamp_millis();

        let rows_affected = sqlx::query(
            r#"
            UPDATE privchat_users
            SET last_active_at = $2, updated_at = $2
            WHERE user_id = $1
            "#,
        )
        .bind(user_id as i64)
        .bind(now)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to update last login: {}", e)))?;

        if rows_affected.rows_affected() == 0 {
            return Err(DatabaseError::NotFound("User not found".to_string()));
        }

        Ok(())
    }

    /// 获取所有用户（分页，管理 API）
    pub async fn find_all_paginated(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<User>, u32), DatabaseError> {
        let offset = (page - 1) * page_size;

        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            ORDER BY created_at DESC
            LIMIT $1 OFFSET $2
            "#,
        )
        .bind(page_size as i64)
        .bind(offset as i64)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query users: {}", e)))?;

        let users: Vec<User> = rows
            .into_iter()
            .map(|r| {
                User::from_db_row(
                    r.user_id,
                    r.username,
                    r.password_hash,
                    r.phone,
                    r.email,
                    r.display_name,
                    r.avatar_url,
                    r.user_type.unwrap_or(0),
                    r.status.unwrap_or(0),
                    r.privacy_settings
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.created_at,
                    r.updated_at,
                    r.last_active_at,
                )
            })
            .collect();

        // 统计总数
        let total_result: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM privchat_users")
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to count users: {}", e)))?;

        let total = total_result.0 as u32;

        Ok((users, total))
    }

    /// 获取所有用户
    pub async fn find_all(&self) -> Result<Vec<User>, DatabaseError> {
        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to query users: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                User::from_db_row(
                    r.user_id,
                    r.username,
                    r.password_hash,
                    r.phone,
                    r.email,
                    r.display_name,
                    r.avatar_url,
                    r.user_type.unwrap_or(0),
                    r.status.unwrap_or(0),
                    r.privacy_settings
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.created_at,
                    r.updated_at,
                    r.last_active_at,
                )
            })
            .collect())
    }

    /// 搜索用户
    pub async fn search(&self, query: &str) -> Result<Vec<User>, DatabaseError> {
        let search_pattern = format!("%{}%", query);

        #[derive(sqlx::FromRow)]
        struct UserRow {
            user_id: i64,
            username: String,
            password_hash: Option<String>,
            phone: Option<String>,
            email: Option<String>,
            display_name: Option<String>,
            avatar_url: Option<String>,
            user_type: Option<i16>,
            status: Option<i16>,
            privacy_settings: Option<serde_json::Value>,
            created_at: i64,
            updated_at: i64,
            last_active_at: Option<i64>,
        }

        let rows = sqlx::query_as::<_, UserRow>(
            r#"
            SELECT 
                user_id,
                username,
                password_hash,
                phone,
                email,
                display_name,
                avatar_url,
                user_type,
                status,
                privacy_settings,
                created_at,
                updated_at,
                last_active_at
            FROM privchat_users
            WHERE 
                LOWER(username) LIKE LOWER($1)
                OR LOWER(display_name) LIKE LOWER($1)
            ORDER BY created_at DESC
            LIMIT 50
            "#,
        )
        .bind(search_pattern)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to search users: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| {
                User::from_db_row(
                    r.user_id,
                    r.username,
                    r.password_hash,
                    r.phone,
                    r.email,
                    r.display_name,
                    r.avatar_url,
                    r.user_type.unwrap_or(0),
                    r.status.unwrap_or(0),
                    r.privacy_settings
                        .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                    r.created_at,
                    r.updated_at,
                    r.last_active_at,
                )
            })
            .collect())
    }

    /// 获取用户数量
    pub async fn count(&self) -> Result<usize, DatabaseError> {
        let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM privchat_users")
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| DatabaseError::Database(format!("Failed to count users: {}", e)))?;

        Ok(row.0 as usize)
    }

    /// 检查用户是否存在
    pub async fn exists(&self, user_id: u64) -> Result<bool, DatabaseError> {
        let row: Option<(i32,)> = sqlx::query_as("SELECT 1 FROM privchat_users WHERE user_id = $1")
            .bind(user_id as i64)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| {
                DatabaseError::Database(format!("Failed to check user existence: {}", e))
            })?;

        Ok(row.is_some())
    }

    /// 删除用户（管理 API）
    ///
    /// 注意：这里使用软删除，将用户状态设置为禁用
    pub async fn delete(&self, user_id: u64) -> Result<(), DatabaseError> {
        // 使用软删除：将用户状态设置为禁用（status = 2）
        sqlx::query!(
            r#"
            UPDATE privchat_users
            SET status = 2, updated_at = $1
            WHERE user_id = $2
            "#,
            chrono::Utc::now().timestamp_millis(),
            user_id as i64,
        )
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| DatabaseError::Database(format!("Failed to delete user: {}", e)))?;

        Ok(())
    }
}
