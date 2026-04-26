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

//! Admin Service — 管理 API 的业务编排层
//!
//! 负责跨多个 service/repository 的管理操作编排。
//! Route handler 只做请求解析 + 调用 AdminService + 构建响应。

use std::sync::Arc;
use tracing::info;

use crate::auth::DeviceManagerDb;
use crate::error::{Result, ServerError};
use crate::infra::ConnectionManager;
use crate::model::user::UserStatus;
use crate::repository::{PgMessageRepository, UserRepository};
use crate::service::ChannelService;

/// Admin Service
pub struct AdminService {
    user_repository: Arc<UserRepository>,
    device_manager_db: Arc<DeviceManagerDb>,
    connection_manager: Arc<ConnectionManager>,
    channel_service: Arc<ChannelService>,
    message_repository: Arc<PgMessageRepository>,
}

/// 封禁用户结果
pub struct SuspendUserResult {
    pub previous_status: i16,
    pub revoked_devices: usize,
}

/// 解封用户结果
pub struct UnsuspendUserResult {
    pub previous_status: i16,
}

/// 添加群成员结果
pub struct AddGroupMemberResult {
    /// 系统公告消息 ID（如果发送了公告）
    pub announcement_message_id: Option<u64>,
}

impl AdminService {
    pub fn new(
        user_repository: Arc<UserRepository>,
        device_manager_db: Arc<DeviceManagerDb>,
        connection_manager: Arc<ConnectionManager>,
        channel_service: Arc<ChannelService>,
        message_repository: Arc<PgMessageRepository>,
    ) -> Self {
        Self {
            user_repository,
            device_manager_db,
            connection_manager,
            channel_service,
            message_repository,
        }
    }

    /// 封禁用户
    ///
    /// 1. 将用户状态设为 Suspended
    /// 2. 撤销该用户的所有活跃设备
    /// 3. 断开所有在线连接
    pub async fn suspend_user(&self, user_id: u64, reason: &str) -> Result<SuspendUserResult> {
        // 1. 查找用户
        let mut user = self
            .user_repository
            .find_by_id(user_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询用户失败: {}", e)))?
            .ok_or_else(|| ServerError::NotFound(format!("用户 {} 不存在", user_id)))?;

        let previous_status = user.status.to_i16();

        // 2. 设置为 Suspended
        user.status = UserStatus::Suspended;
        self.user_repository
            .update(&user)
            .await
            .map_err(|e| ServerError::Database(format!("更新用户状态失败: {}", e)))?;

        // 3. 撤销该用户的所有设备
        let revoked_devices = self
            .device_manager_db
            .revoke_all_devices(user_id, reason)
            .await
            .unwrap_or(0);

        // 4. 断开在线连接
        let _ = self
            .connection_manager
            .disconnect_other_devices(user_id, "__none__")
            .await;

        info!(
            "✅ 用户已封禁: user_id={}, revoked_devices={}, reason={}",
            user_id, revoked_devices, reason
        );

        Ok(SuspendUserResult {
            previous_status,
            revoked_devices,
        })
    }

    /// 解封用户
    ///
    /// 将用户状态恢复为 Active
    pub async fn unsuspend_user(&self, user_id: u64) -> Result<UnsuspendUserResult> {
        let mut user = self
            .user_repository
            .find_by_id(user_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询用户失败: {}", e)))?
            .ok_or_else(|| ServerError::NotFound(format!("用户 {} 不存在", user_id)))?;

        let previous_status = user.status.to_i16();

        user.status = UserStatus::Active;
        self.user_repository
            .update(&user)
            .await
            .map_err(|e| ServerError::Database(format!("更新用户状态失败: {}", e)))?;

        info!("✅ 用户已解封: user_id={}", user_id);

        Ok(UnsuspendUserResult { previous_status })
    }

    /// 踢出指定设备
    ///
    /// 1. 数据库层面标记设备被踢
    /// 2. 断开在线连接
    pub async fn revoke_device(&self, user_id: u64, device_id: &str, reason: &str) -> Result<()> {
        self.device_manager_db
            .kick_device(user_id, device_id, None, reason)
            .await?;

        let _ = self
            .connection_manager
            .disconnect_device(user_id, device_id)
            .await;

        info!(
            "✅ 设备已踢出: device_id={}, user_id={}, reason={}",
            device_id, user_id, reason
        );

        Ok(())
    }

    /// 撤销用户全部设备
    ///
    /// 1. 数据库层面撤销所有设备
    /// 2. 断开所有在线连接
    pub async fn revoke_all_devices(&self, user_id: u64, reason: &str) -> Result<usize> {
        let revoked_count = self
            .device_manager_db
            .revoke_all_devices(user_id, reason)
            .await?;

        let _ = self
            .connection_manager
            .disconnect_other_devices(user_id, "__none__")
            .await;

        info!(
            "✅ 用户全部设备已撤销: user_id={}, count={}, reason={}",
            user_id, revoked_count, reason
        );

        Ok(revoked_count)
    }

    /// 添加用户到群组并发送系统公告
    ///
    /// 1. 验证用户存在
    /// 2. 添加用户到群组（DB + 内存）
    /// 3. 发送系统公告 "[XXX加入了本群]"
    pub async fn add_user_to_group_with_announcement(
        &self,
        group_id: u64,
        user_id: u64,
    ) -> Result<AddGroupMemberResult> {
        // 1. 查找用户（获取用户名用于公告）
        let user = self
            .user_repository
            .find_by_id(user_id)
            .await
            .map_err(|e| ServerError::Database(format!("查询用户失败: {}", e)))?
            .ok_or_else(|| ServerError::NotFound(format!("用户 {} 不存在", user_id)))?;

        let username_fallback = user.username_or_default();
        let display_name = user.display_name.as_deref().unwrap_or(&username_fallback);

        // 2. 添加用户到群组
        self.channel_service
            .add_member_admin(group_id, user_id)
            .await?;

        // 3. 查找群组对应的 channel_id（group_id 就是 channel_id）
        let channel_id = group_id;

        // 4. 发送系统公告
        let announcement = format!("{}加入了本群", display_name);
        let metadata = serde_json::json!({
            "type": "member_joined",
            "user_id": user_id,
            "display_name": display_name,
        });

        let (message_id, _created_at) = self
            .message_repository
            .send_system_message_admin(channel_id, &announcement, 10, &metadata)
            .await
            .map_err(|e| ServerError::Database(format!("发送入群公告失败: {}", e)))?;

        info!(
            "✅ 用户已加入群组: group_id={}, user_id={}, announcement_msg={}",
            group_id, user_id, message_id
        );

        Ok(AddGroupMemberResult {
            announcement_message_id: Some(message_id),
        })
    }
}
