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

use crate::error::{Result, ServerError};
use crate::push::types::PushVendor;
use sqlx::PgPool;

/// 用户设备信息（用于推送）
#[derive(Debug, Clone)]
pub struct UserDevice {
    pub id: i64,
    pub user_id: u64,
    pub device_id: String,
    pub platform: String,
    pub vendor: PushVendor,
    pub push_token: Option<String>,
    pub apns_armed: bool, // ✨ Phase 3.5: 是否需要推送
    pub connected: bool,  // ✨ Phase 3.5: 是否已连接
}

/// 用户设备 Repository
pub struct UserDeviceRepository {
    pool: PgPool,
}

impl UserDeviceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 获取用户的所有设备
    pub async fn get_user_devices(&self, user_id: u64) -> Result<Vec<UserDevice>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: i64,
            user_id: i64,
            device_id: String,
            platform: String,
            vendor: String,
            push_token: Option<String>,
            apns_armed: Option<bool>, // ✨ Phase 3.5
            connected: Option<bool>,  // ✨ Phase 3.5
        }

        let rows = sqlx::query_as::<_, Row>(
            r#"
            SELECT 
                id,
                user_id,
                device_id,
                platform,
                vendor,
                push_token,
                apns_armed,
                connected
            FROM privchat_user_devices
            WHERE user_id = $1
            "#,
        )
        .bind(user_id as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户设备失败: {}", e)))?;

        let devices = rows
            .into_iter()
            .filter_map(|row| {
                // 只返回有 push_token 的设备
                if row.push_token.is_none() {
                    return None;
                }

                let vendor = match PushVendor::from_str(&row.vendor) {
                    Some(v) => v,
                    None => {
                        tracing::warn!("Unknown vendor: {}", row.vendor);
                        return None;
                    }
                };

                Some(UserDevice {
                    id: row.id,
                    user_id: row.user_id as u64,
                    device_id: row.device_id,
                    platform: row.platform,
                    vendor,
                    push_token: row.push_token,
                    apns_armed: row.apns_armed.unwrap_or(false), // ✨ Phase 3.5
                    connected: row.connected.unwrap_or(false),   // ✨ Phase 3.5
                })
            })
            .collect();

        Ok(devices)
    }

    /// ✨ Phase 3.5: 获取单个设备
    pub async fn get_device(&self, user_id: u64, device_id: &str) -> Result<Option<UserDevice>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            id: i64,
            user_id: i64,
            device_id: String,
            platform: String,
            vendor: String,
            push_token: Option<String>,
            apns_armed: Option<bool>,
            connected: Option<bool>,
        }

        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT 
                id,
                user_id,
                device_id,
                platform,
                vendor,
                push_token,
                apns_armed,
                connected
            FROM privchat_user_devices
            WHERE user_id = $1 AND device_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备失败: {}", e)))?;

        if let Some(row) = row {
            let vendor = PushVendor::from_str(&row.vendor)
                .ok_or_else(|| ServerError::Internal(format!("Unknown vendor: {}", row.vendor)))?;

            Ok(Some(UserDevice {
                id: row.id,
                user_id: row.user_id as u64,
                device_id: row.device_id,
                platform: row.platform,
                vendor,
                push_token: row.push_token,
                apns_armed: row.apns_armed.unwrap_or(false),
                connected: row.connected.unwrap_or(false),
            }))
        } else {
            Ok(None)
        }
    }

    /// ✨ Phase 3.5: 更新设备推送状态
    pub async fn update_device_push_state(
        &self,
        user_id: u64,
        device_id: &str,
        apns_armed: bool,
        push_token: Option<&str>,
        vendor: Option<&str>,
    ) -> Result<()> {
        let platform = self
            .query_platform(user_id, device_id)
            .await?
            .unwrap_or_else(|| "unknown".to_string());
        let inferred_vendor = Self::resolve_vendor(vendor, &platform)?;
        let token = push_token.map(|it| it.trim()).filter(|it| !it.is_empty());

        // 使用 UPSERT，避免设备首次上报时行不存在导致更新无效。
        sqlx::query(
            r#"
            INSERT INTO privchat_user_devices (user_id, device_id, platform, vendor, push_token, apns_armed, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, NOW())
            ON CONFLICT (user_id, device_id)
            DO UPDATE SET
                platform = EXCLUDED.platform,
                vendor = EXCLUDED.vendor,
                push_token = COALESCE(EXCLUDED.push_token, privchat_user_devices.push_token),
                apns_armed = EXCLUDED.apns_armed,
                updated_at = NOW()
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .bind(&platform)
        .bind(inferred_vendor.as_str())
        .bind(token)
        .bind(apns_armed)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("更新设备推送状态失败: {}", e)))?;

        Ok(())
    }

    async fn query_platform(&self, user_id: u64, device_id: &str) -> Result<Option<String>> {
        let row = sqlx::query_scalar::<_, String>(
            r#"
            SELECT platform
            FROM privchat_user_devices
            WHERE user_id = $1 AND device_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备平台失败: {}", e)))?;

        if row.is_some() {
            return Ok(row);
        }

        let device_type = sqlx::query_scalar::<_, String>(
            r#"
            SELECT device_type
            FROM privchat_devices
            WHERE user_id = $1 AND device_id::text = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询认证设备平台失败: {}", e)))?;

        Ok(device_type)
    }

    fn resolve_vendor(vendor: Option<&str>, platform: &str) -> Result<PushVendor> {
        if let Some(raw_vendor) = vendor.map(str::trim).filter(|it| !it.is_empty()) {
            return PushVendor::from_str(raw_vendor).ok_or_else(|| {
                ServerError::BadRequest(format!("不支持的推送 vendor: {}", raw_vendor))
            });
        }

        let resolved = match platform.to_ascii_lowercase().as_str() {
            "ios" | "macos" => PushVendor::Apns,
            "android" => PushVendor::Fcm,
            _ => PushVendor::Fcm, // 默认按 Android/GMS 处理，后续客户端可显式上报 vendor 覆盖
        };
        Ok(resolved)
    }

    /// ✨ Phase 3.5: 检查用户是否所有设备都需要推送
    pub async fn check_user_push_enabled(&self, user_id: u64) -> Result<bool> {
        #[derive(sqlx::FromRow)]
        struct Row {
            total_devices: Option<i64>,
            armed_devices: Option<i64>,
        }

        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT 
                COUNT(*) as total_devices,
                COUNT(*) FILTER (WHERE apns_armed = true) as armed_devices
            FROM privchat_user_devices
            WHERE user_id = $1
            "#,
        )
        .bind(user_id as i64)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询用户推送状态失败: {}", e)))?;

        let total = row.total_devices.unwrap_or(0);
        let armed = row.armed_devices.unwrap_or(0);

        // 如果所有设备都需要推送，返回 true
        Ok(total > 0 && total == armed)
    }

    /// ✨ Phase 3.5: 更新设备连接状态
    pub async fn update_device_connected(
        &self,
        user_id: u64,
        device_id: &str,
        connected: bool,
    ) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE privchat_user_devices
            SET 
                connected = $1,
                updated_at = NOW()
            WHERE user_id = $2 AND device_id = $3
            "#,
        )
        .bind(connected)
        .bind(user_id as i64)
        .bind(device_id)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("更新设备连接状态失败: {}", e)))?;

        Ok(())
    }

    /// 注册或更新设备推送令牌
    pub async fn register_or_update_device(
        &self,
        user_id: u64,
        device_id: &str,
        platform: &str,
        vendor: &str,
        push_token: &str,
    ) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO privchat_user_devices (user_id, device_id, platform, vendor, push_token)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (user_id, device_id)
            DO UPDATE SET
                platform = EXCLUDED.platform,
                vendor = EXCLUDED.vendor,
                push_token = EXCLUDED.push_token,
                updated_at = NOW()
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .bind(platform)
        .bind(vendor)
        .bind(push_token)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("注册设备失败: {}", e)))?;

        Ok(())
    }

    /// 注销设备
    pub async fn unregister_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        sqlx::query(
            r#"
            DELETE FROM privchat_user_devices
            WHERE user_id = $1 AND device_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("注销设备失败: {}", e)))?;

        Ok(())
    }
}
