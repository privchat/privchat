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

use std::sync::Arc;

use tracing::info;

use crate::error::{DatabaseError, ServerError};
use crate::model::user::{User, UserStatus};
use crate::repository::UserRepository;

/// admin 创建用户的参数集合
///
/// service 内部会做用户名/邮箱的 normalize + 唯一性校验 + 记录持久化。
#[derive(Debug, Clone)]
pub struct CreateUserAdminParams {
    pub username: String,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub avatar_url: Option<String>,
    pub user_type: Option<i16>,
}

/// admin 更新用户的参数集合
///
/// 字段均为可选：`None` 表示不改动；`Some(value)` 表示落库。
/// `email: Some("".to_string())` 在 normalize 后会变成 `None` 并清空邮箱。
#[derive(Debug, Clone, Default)]
pub struct UpdateUserAdminParams {
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub avatar_url: Option<String>,
    pub status: Option<i16>,
}

/// 用户服务
///
/// 按 ADMIN_API_SPEC §1.4 的 Global Service Convergence Rule，所有 admin / RPC /
/// job 入口对 User 领域对象的操作都经过本服务，admin 入口不得直连 `UserRepository`。
pub struct UserService {
    user_repository: Arc<UserRepository>,
}

impl UserService {
    pub fn new(user_repository: Arc<UserRepository>) -> Self {
        Self { user_repository }
    }

    pub async fn find_by_id(&self, user_id: u64) -> Result<Option<User>, ServerError> {
        self.user_repository
            .find_by_id(user_id)
            .await
            .map_err(map_db_error_query)
    }

    pub async fn find_by_email(&self, email: &str) -> Result<Option<User>, ServerError> {
        self.user_repository
            .find_by_email(email)
            .await
            .map_err(map_db_error_query)
    }

    pub async fn find_by_username(&self, username: &str) -> Result<Option<User>, ServerError> {
        self.user_repository
            .find_by_username(username)
            .await
            .map_err(map_db_error_query)
    }

    pub async fn search(&self, query: &str) -> Result<Vec<User>, ServerError> {
        self.user_repository
            .search(query)
            .await
            .map_err(|e| ServerError::Database(format!("搜索用户失败: {}", e)))
    }

    pub async fn find_all_paginated(
        &self,
        page: u32,
        page_size: u32,
    ) -> Result<(Vec<User>, u32), ServerError> {
        self.user_repository
            .find_all_paginated(page, page_size)
            .await
            .map_err(|e| ServerError::Database(format!("获取用户列表失败: {}", e)))
    }

    pub async fn count(&self) -> Result<usize, ServerError> {
        self.user_repository
            .count()
            .await
            .map_err(|e| ServerError::Database(format!("统计用户数失败: {}", e)))
    }

    pub async fn exists(&self, user_id: u64) -> Result<bool, ServerError> {
        self.user_repository
            .exists(user_id)
            .await
            .map_err(|e| ServerError::Database(format!("检查用户失败: {}", e)))
    }

    /// admin 创建用户：normalize + 唯一性预检 + 持久化。
    pub async fn create_user_admin(
        &self,
        params: CreateUserAdminParams,
    ) -> Result<User, ServerError> {
        let CreateUserAdminParams {
            username,
            display_name,
            email,
            phone,
            avatar_url,
            user_type,
        } = params;

        let normalized_username = username.trim().to_lowercase();
        if normalized_username.is_empty() {
            return Err(ServerError::Validation("用户名不能为空".to_string()));
        }

        let normalized_email = email
            .as_ref()
            .map(|val| val.trim().to_lowercase())
            .filter(|val| !val.is_empty());

        if self.find_by_username(&normalized_username).await?.is_some() {
            return Err(ServerError::Validation(format!(
                "用户名 {} 已存在",
                normalized_username
            )));
        }

        if let Some(ref email_val) = normalized_email {
            if self.find_by_email(email_val).await?.is_some() {
                return Err(ServerError::Validation(format!("邮箱 {} 已存在", email_val)));
            }
        }

        let mut user = User::new_with_details(
            0,
            normalized_username,
            display_name,
            normalized_email,
            avatar_url,
        );
        if let Some(phone) = phone {
            user.phone = Some(phone);
        }
        if let Some(user_type) = user_type {
            user.user_type = user_type;
        }

        let created = self
            .user_repository
            .create(&user)
            .await
            .map_err(|e| match e {
                DatabaseError::DuplicateEntry(msg) => ServerError::Validation(msg),
                other => ServerError::Database(format!("创建用户失败: {}", other)),
            })?;

        info!(
            "✅ 用户创建成功: user_id={}, username={}",
            created.id, created.username
        );

        Ok(created)
    }

    /// admin 更新用户：拉取现有用户 -> 按 patch 语义 merge -> 持久化。
    ///
    /// 邮箱变更会检查除本人外是否存在冲突占用；空字符串视为清空。
    pub async fn update_user_admin(
        &self,
        user_id: u64,
        params: UpdateUserAdminParams,
    ) -> Result<User, ServerError> {
        let mut user = self
            .find_by_id(user_id)
            .await?
            .ok_or_else(|| ServerError::NotFound(format!("用户 {} 不存在", user_id)))?;

        let UpdateUserAdminParams {
            display_name,
            email,
            phone,
            avatar_url,
            status,
        } = params;

        if let Some(display_name) = display_name {
            user.display_name = Some(display_name);
        }

        if let Some(email) = email {
            let normalized_email = email.trim().to_lowercase();
            if normalized_email.is_empty() {
                user.email = None;
            } else {
                if let Some(existing) = self.find_by_email(&normalized_email).await? {
                    if existing.id != user_id {
                        return Err(ServerError::Validation(format!(
                            "邮箱 {} 已存在",
                            normalized_email
                        )));
                    }
                }
                user.email = Some(normalized_email);
            }
        }

        if let Some(phone) = phone {
            user.phone = Some(phone);
        }
        if let Some(avatar_url) = avatar_url {
            user.avatar_url = Some(avatar_url);
        }
        if let Some(status) = status {
            user.status = UserStatus::from_i16(status);
        }

        self.user_repository
            .update(&user)
            .await
            .map_err(|e| ServerError::Database(format!("更新用户失败: {}", e)))?;

        Ok(user)
    }

    /// admin 软删除用户：exists 前置校验 + 调 repository.delete。
    pub async fn delete_user_admin(&self, user_id: u64) -> Result<(), ServerError> {
        if !self.exists(user_id).await? {
            return Err(ServerError::NotFound(format!("用户 {} 不存在", user_id)));
        }

        self.user_repository
            .delete(user_id)
            .await
            .map_err(|e| ServerError::Database(format!("删除用户失败: {}", e)))?;

        info!("✅ 用户已删除（软删除）: user_id={}", user_id);
        Ok(())
    }
}

fn map_db_error_query(err: DatabaseError) -> ServerError {
    ServerError::Database(format!("查询用户失败: {}", err))
}
