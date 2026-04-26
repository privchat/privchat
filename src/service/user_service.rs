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
/// v1.2 service API：所有标识字段（username / phone / email）均可选；
/// 三者全空也合法（创建占位 uid，所有标识字段落 NULL）。
/// service 内部会做 normalize + 多键幂等检测 + 记录持久化。
#[derive(Debug, Clone, Default)]
pub struct CreateUserAdminParams {
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub avatar_url: Option<String>,
    pub user_type: Option<i16>,
    pub business_system_id: Option<String>,
}

/// `create_user_admin` 的执行结果。
///
/// `Created`：本次新建用户；`Existing`：因某个标识字段（phone/email/username）已存在，
/// 直接返回已有用户，**不**对 existing 做任何字段覆盖。
pub enum CreateUserOutcome {
    Created(User),
    Existing(User),
}

impl CreateUserOutcome {
    pub fn into_user(self) -> User {
        match self {
            CreateUserOutcome::Created(u) | CreateUserOutcome::Existing(u) => u,
        }
    }

    pub fn user(&self) -> &User {
        match self {
            CreateUserOutcome::Created(u) | CreateUserOutcome::Existing(u) => u,
        }
    }

    pub fn was_created(&self) -> bool {
        matches!(self, CreateUserOutcome::Created(_))
    }
}

/// E.164 手机号格式校验：`+` 开头 + 首位非 0 + 1-14 位数字。
///
/// 不做猜测式规范化（如自动加 `+86`）；不合法直接拒绝。
pub fn validate_phone_e164(phone: &str) -> bool {
    let bytes = phone.as_bytes();
    if bytes.len() < 3 || bytes.len() > 16 {
        return false;
    }
    if bytes[0] != b'+' {
        return false;
    }
    if !(b'1'..=b'9').contains(&bytes[1]) {
        return false;
    }
    bytes[2..].iter().all(|b| b.is_ascii_digit())
}

#[cfg(test)]
mod phone_validator_tests {
    use super::validate_phone_e164;

    #[test]
    fn accepts_valid_e164() {
        assert!(validate_phone_e164("+8613800000000"));
        assert!(validate_phone_e164("+14155552671"));
        assert!(validate_phone_e164("+447911123456"));
        assert!(validate_phone_e164("+1234567890123456".get(..16).unwrap()));
    }

    #[test]
    fn rejects_missing_plus() {
        assert!(!validate_phone_e164("8613800000000"));
        assert!(!validate_phone_e164("13800000000"));
    }

    #[test]
    fn rejects_leading_zero_country_code() {
        assert!(!validate_phone_e164("+0613800000000"));
    }

    #[test]
    fn rejects_non_digit_body() {
        assert!(!validate_phone_e164("+86 138 0000 0000"));
        assert!(!validate_phone_e164("+86-138-0000"));
        assert!(!validate_phone_e164("+86abc"));
    }

    #[test]
    fn rejects_too_short_or_too_long() {
        assert!(!validate_phone_e164(""));
        assert!(!validate_phone_e164("+"));
        assert!(!validate_phone_e164("+1"));
        // > 16 chars (E.164 max = + plus 15 digits)
        assert!(!validate_phone_e164("+12345678901234567"));
    }

    #[test]
    fn rejects_only_plus() {
        assert!(!validate_phone_e164("+"));
    }
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

    pub async fn find_by_phone(&self, phone: &str) -> Result<Option<User>, ServerError> {
        self.user_repository
            .find_by_phone(phone)
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

    /// v1.2 service API 创建用户：多键幂等 + E.164 phone 校验 + 字段全可选。
    ///
    /// 行为：
    /// - normalize: username/email trim + lowercase；phone trim；E.164 格式校验
    /// - 幂等优先级：phone → email → username。任一命中已有用户即返回 `Existing(user)`，
    ///   不覆盖任何字段
    /// - 全空（phone/email/username 都 None）合法，创建占位 uid
    /// - INSERT UNIQUE 冲突兜底：catch 后按 phone/email/username 顺序 re-find，
    ///   返回 existing；找不到对应 row 才返回错误（极端竞态）
    pub async fn create_user_admin(
        &self,
        params: CreateUserAdminParams,
    ) -> Result<CreateUserOutcome, ServerError> {
        let CreateUserAdminParams {
            username,
            display_name,
            email,
            phone,
            avatar_url,
            user_type,
            business_system_id,
        } = params;

        // 1. normalize
        let normalized_phone = phone
            .as_ref()
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty());
        let normalized_email = email
            .as_ref()
            .map(|val| val.trim().to_lowercase())
            .filter(|val| !val.is_empty());
        let normalized_username = username
            .as_ref()
            .map(|val| val.trim().to_lowercase())
            .filter(|val| !val.is_empty());

        // 2. phone 强制 E.164
        if let Some(ref phone_val) = normalized_phone {
            if !validate_phone_e164(phone_val) {
                return Err(ServerError::Validation(format!(
                    "INVALID_PHONE_FORMAT: phone must be E.164 (e.g. +8613800000000), got {}",
                    phone_val
                )));
            }
        }

        // 3. 幂等预检：phone → email → username
        if let Some(ref phone_val) = normalized_phone {
            if let Some(existing) = self.find_by_phone(phone_val).await? {
                info!(
                    "ℹ️ 幂等命中（phone）: user_id={}, phone={}",
                    existing.id, phone_val
                );
                return Ok(CreateUserOutcome::Existing(existing));
            }
        }
        if let Some(ref email_val) = normalized_email {
            if let Some(existing) = self.find_by_email(email_val).await? {
                info!(
                    "ℹ️ 幂等命中（email）: user_id={}, email={}",
                    existing.id, email_val
                );
                return Ok(CreateUserOutcome::Existing(existing));
            }
        }
        if let Some(ref username_val) = normalized_username {
            if let Some(existing) = self.find_by_username(username_val).await? {
                info!(
                    "ℹ️ 幂等命中（username）: user_id={}, username={}",
                    existing.id, username_val
                );
                return Ok(CreateUserOutcome::Existing(existing));
            }
        }

        // 4. 构造新 user 并持久化
        let user = User::new_admin(
            0,
            normalized_username.clone(),
            normalized_phone.clone(),
            normalized_email.clone(),
            display_name,
            avatar_url,
            user_type.unwrap_or(0),
            business_system_id,
        );

        match self.user_repository.create(&user).await {
            Ok(created) => {
                info!(
                    "✅ 用户创建成功: user_id={}, username={}, phone={}",
                    created.id,
                    created.username.as_deref().unwrap_or("<none>"),
                    created.phone.as_deref().unwrap_or("<none>")
                );
                Ok(CreateUserOutcome::Created(created))
            }
            Err(DatabaseError::DuplicateEntry(msg)) => {
                // 并发兜底：另一个请求在我们 pre-check 后抢先 INSERT，按相同优先级 re-find
                if let Some(ref phone_val) = normalized_phone {
                    if let Some(existing) = self.find_by_phone(phone_val).await? {
                        info!(
                            "ℹ️ 并发幂等命中（phone）: user_id={}, phone={}",
                            existing.id, phone_val
                        );
                        return Ok(CreateUserOutcome::Existing(existing));
                    }
                }
                if let Some(ref email_val) = normalized_email {
                    if let Some(existing) = self.find_by_email(email_val).await? {
                        info!(
                            "ℹ️ 并发幂等命中（email）: user_id={}, email={}",
                            existing.id, email_val
                        );
                        return Ok(CreateUserOutcome::Existing(existing));
                    }
                }
                if let Some(ref username_val) = normalized_username {
                    if let Some(existing) = self.find_by_username(username_val).await? {
                        info!(
                            "ℹ️ 并发幂等命中（username）: user_id={}, username={}",
                            existing.id, username_val
                        );
                        return Ok(CreateUserOutcome::Existing(existing));
                    }
                }
                Err(ServerError::Validation(msg))
            }
            Err(other) => Err(ServerError::Database(format!("创建用户失败: {}", other))),
        }
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
