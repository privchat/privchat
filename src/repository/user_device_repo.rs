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
    /// 设备语言（BCP-47）。None = 老客户端没上报，按简体中文兜底。
    pub locale: Option<String>,
    /// 这台设备的远程通知是否带提示音。
    pub push_sound: bool,
}

/// 推送偏好在 `privchat_user_settings` 里的 key。
pub const PUSH_PREFERENCE_KEY: &str = "notifications.push";

/// 账号级推送偏好。跨设备一致，所以存用户设置而不是设备表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushPreference {
    /// 通知里是否显示消息内容。false = 只显示"你收到一条新消息"。
    pub show_preview: bool,
    /// 全局免打扰：所有会话都不推。
    pub global_mute: bool,
}

impl Default for PushPreference {
    fn default() -> Self {
        // 默认显示预览、不全局静音——与客户端本地通知设置的默认值一致，
        // 两边不一致的话用户会看到"App 内不显示预览、锁屏却显示"。
        Self {
            show_preview: true,
            global_mute: false,
        }
    }
}

impl PushPreference {
    /// 从 `value_json` 解析。缺字段/解析失败一律回默认值——没设置过就是默认。
    pub fn from_json(value: Option<&serde_json::Value>) -> Self {
        let default = Self::default();
        let Some(value) = value else { return default };
        Self {
            show_preview: value
                .get("showPreview")
                .and_then(|v| v.as_bool())
                .unwrap_or(default.show_preview),
            global_mute: value
                .get("globalMute")
                .and_then(|v| v.as_bool())
                .unwrap_or(default.global_mute),
        }
    }
}

/// 生成推送时需要的收件人侧状态快照。
#[derive(Debug, Clone, Copy)]
pub struct PushContext {
    pub muted: bool,
    pub unread_total: i64,
    pub show_preview: bool,
    pub global_mute: bool,
}

impl Default for PushContext {
    fn default() -> Self {
        let preference = PushPreference::default();
        Self {
            muted: false,
            unread_total: 0,
            show_preview: preference.show_preview,
            global_mute: preference.global_mute,
        }
    }
}

/// 用户设备 Repository
pub struct UserDeviceRepository {
    pool: PgPool,
}

impl UserDeviceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// 用户当前的未读消息总数（跨所有会话），用于 iOS 角标。
    ///
    /// 走 `idx_privchat_user_channels_unread` 这个部分索引（只覆盖 unread_count > 0），
    /// 扫的是"这个用户有未读的那几个会话"，不是全部会话。
    ///
    /// 查询失败返回 0：角标数字不对是小事，为它把整条推送挡掉才是大事。iOS 侧
    /// badge=0 会清掉角标——这一点在下面调用处有兜底（0 时不带 badge 字段）。
    pub async fn total_unread_count(&self, user_id: u64) -> i64 {
        sqlx::query_scalar::<_, Option<i64>>(
            r#"
            SELECT COALESCE(SUM(unread_count), 0)::bigint
            FROM privchat_user_channels
            WHERE user_id = $1 AND unread_count > 0
            "#,
        )
        .bind(user_id as i64)
        .fetch_one(&self.pool)
        .await
        .ok()
        .flatten()
        .unwrap_or(0)
    }

    /// 清掉一个已被 provider 判定为失效的 push token。
    ///
    /// 只清 token 并把 apns_armed 置 false，不删设备行：设备的其它状态（platform、
    /// 上次连接时间）还有用，而且用户重装 App 后会带着新 token 再来 upsert 同一行。
    ///
    /// 带上 `push_token = $3` 的条件是防止竞态：провайдер 报失效和客户端上报新 token
    /// 可能同时发生，不加这个条件就会把刚拿到的新 token 抹掉。
    pub async fn invalidate_push_token(
        &self,
        user_id: u64,
        device_id: &str,
        stale_token: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            r#"
            UPDATE privchat_user_devices
            SET push_token = NULL,
                apns_armed = false,
                updated_at = NOW()
            WHERE user_id = $1 AND device_id = $2 AND push_token = $3
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .bind(stale_token)
        .execute(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("清理失效 push token 失败: {}", e)))?;

        if result.rows_affected() > 0 {
            tracing::info!(
                "已清理失效 push token: user={} device={}",
                user_id,
                device_id
            );
        }
        Ok(())
    }

    /// 生成一条推送需要的全部收件人侧状态，一次查询取回。
    ///
    /// 以前是三次独立往返（免打扰、未读总数、推送偏好），而这条路径对**每个离线
    /// 收件人**都要走一遍——群聊里就是每人三次。合成一次之后，判定所依据的也是
    /// 同一时刻的快照，不会出现"读到的免打扰是新的、未读数是旧的"。
    ///
    /// 🔴 **查询失败返回 Err，调用方必须放弃这次推送。**
    ///
    /// "查不到就按默认值推"是错的：默认值是"显示预览、不静音"，于是数据库抖一下，
    /// 一个已经关掉预览的用户就会在锁屏上看到消息正文，一个开了全局免打扰的用户
    /// 就会被吵醒。这两件事都不可撤销。
    ///
    /// 注意区分**没有设置记录**和**读不到**：前者是"用户没设置过"，用产品默认值是对的
    /// （在 [PushPreference::from_json] 里处理）；后者是"我们不知道用户设了什么"，
    /// 唯一安全的动作是不推。
    pub async fn load_push_context(&self, user_id: u64, channel_id: u64) -> Result<PushContext> {
        #[derive(sqlx::FromRow)]
        struct Row {
            muted: Option<bool>,
            unread_total: Option<i64>,
            preference: Option<serde_json::Value>,
        }

        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT
                (SELECT is_muted FROM privchat_user_channels
                  WHERE user_id = $1 AND channel_id = $2) AS muted,
                (SELECT COALESCE(SUM(unread_count), 0)::bigint FROM privchat_user_channels
                  WHERE user_id = $1 AND unread_count > 0) AS unread_total,
                (SELECT value_json FROM privchat_user_settings
                  WHERE user_id = $1 AND setting_key = $3) AS preference
            "#,
        )
        .bind(user_id as i64)
        .bind(channel_id as i64)
        .bind(PUSH_PREFERENCE_KEY)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            ServerError::Database(format!(
                "查询推送上下文失败(user={} channel={}): {}",
                user_id, channel_id, e
            ))
        })?;

        // 这里的 None 是"这个用户没有这条记录"，不是"查不到"——查不到在上面就返回
        // Err 了。没记录就是没设置过，用产品默认值。
        let preference = PushPreference::from_json(row.preference.as_ref());
        Ok(PushContext {
            muted: row.muted.unwrap_or(false),
            unread_total: row.unread_total.unwrap_or(0),
            show_preview: preference.show_preview,
            global_mute: preference.global_mute,
        })
    }

    /// 读取推送偏好。
    pub async fn get_push_preference(&self, user_id: u64) -> Result<PushPreference> {
        let value = sqlx::query_scalar::<_, serde_json::Value>(
            r#"
            SELECT value_json FROM privchat_user_settings
            WHERE user_id = $1 AND setting_key = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(PUSH_PREFERENCE_KEY)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询推送偏好失败: {}", e)))?;

        Ok(PushPreference::from_json(value.as_ref()))
    }

    /// 原子合并推送偏好，返回合并后的值。
    ///
    /// `None` 的字段保持库里的原值。**必须在一条 SQL 里完成**：先 SELECT 再整体
    /// UPDATE 的话，两台设备同时改不同开关时，两边都读到旧值，后写的那次会把对方的
    /// 改动撤销掉——"字段可选"本身不解决并发，只是把竞态从客户端搬到了服务端。
    ///
    /// jsonb 的 `||` 是右侧覆盖左侧，所以按 `默认值 → 库里的值 → 本次改动` 依次叠加：
    /// 没有记录时得到产品默认值 + 本次改动，有记录时保留未提及的字段。
    pub async fn merge_push_preference(
        &self,
        user_id: u64,
        show_preview: Option<bool>,
        global_mute: Option<bool>,
    ) -> Result<PushPreference> {
        let defaults = PushPreference::default();
        let base = serde_json::json!({
            "showPreview": defaults.show_preview,
            "globalMute": defaults.global_mute,
        });
        // 只把请求里出现的字段拼进 patch；缺席的字段不在 jsonb 里，自然不会覆盖。
        let mut patch = serde_json::Map::new();
        if let Some(v) = show_preview {
            patch.insert("showPreview".to_string(), serde_json::Value::Bool(v));
        }
        if let Some(v) = global_mute {
            patch.insert("globalMute".to_string(), serde_json::Value::Bool(v));
        }
        let patch = serde_json::Value::Object(patch);

        let merged = sqlx::query_scalar::<_, serde_json::Value>(
            r#"
            INSERT INTO privchat_user_settings (user_id, setting_key, value_json, version, updated_at)
            VALUES ($1, $2, $3::jsonb || $4::jsonb, 1, (EXTRACT(epoch FROM now()) * 1000)::bigint)
            ON CONFLICT (user_id, setting_key)
            DO UPDATE SET
                -- 读改写发生在数据库内部的这一行里，两个并发事务会被行锁串起来，
                -- 后到的那个看到的是前一个写完的结果。
                value_json = privchat_user_settings.value_json || $4::jsonb,
                version = privchat_user_settings.version + 1,
                updated_at = (EXTRACT(epoch FROM now()) * 1000)::bigint
            RETURNING value_json
            "#,
        )
        .bind(user_id as i64)
        .bind(PUSH_PREFERENCE_KEY)
        .bind(&base)
        .bind(&patch)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ServerError::Database(format!("保存推送偏好失败: {}", e)))?;

        Ok(PushPreference::from_json(Some(&merged)))
    }

    /// 这个会话对这个用户是不是免打扰。
    ///
    /// 免打扰的语义就是"别来吵我"——只在 App 内不弹本地通知、离线却照样推到锁屏，
    /// 等于没设。没有记录（从没设置过）视为不免打扰。
    ///
    /// 查询失败时返回 `false`（照常推送）：宁可多推一条，也不要因为数据库抖动
    /// 把所有人的推送都静音掉。
    pub async fn is_conversation_muted(&self, user_id: u64, channel_id: u64) -> bool {
        let muted = sqlx::query_scalar::<_, Option<bool>>(
            r#"
            SELECT is_muted
            FROM privchat_user_channels
            WHERE user_id = $1 AND channel_id = $2
            "#,
        )
        .bind(user_id as i64)
        .bind(channel_id as i64)
        .fetch_optional(&self.pool)
        .await;

        match muted {
            Ok(Some(Some(true))) => true,
            Ok(_) => false,
            Err(e) => {
                tracing::warn!(
                    "查询会话免打扰失败(user={} channel={}): {}，按未免打扰处理",
                    user_id,
                    channel_id,
                    e
                );
                false
            }
        }
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
            locale: Option<String>,
            push_sound: Option<bool>,
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
                connected,
                locale,
                push_sound
            FROM privchat_user_devices
            WHERE user_id = $1
              -- apns_armed = false 是用户/客户端明确表示"这台设备现在不要推送"：
              -- 关了系统通知权限、退出登录、切到别的账号都会把它置 false。
              -- 这个字段以前只是被读出来放进结构体，没有任何人看，于是登出之后
              -- 照样收推送。
              AND apns_armed = true
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
                    locale: row.locale,
                    push_sound: row.push_sound.unwrap_or(true),
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
            locale: Option<String>,
            push_sound: Option<bool>,
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
                connected,
                locale,
                push_sound
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
                locale: row.locale,
                push_sound: row.push_sound.unwrap_or(true),
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
        locale: Option<&str>,
        push_sound: Option<bool>,
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
            INSERT INTO privchat_user_devices (user_id, device_id, platform, vendor, push_token, apns_armed, locale, push_sound, updated_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, true), NOW())
            ON CONFLICT (user_id, device_id)
            DO UPDATE SET
                platform = EXCLUDED.platform,
                vendor = EXCLUDED.vendor,
                push_token = COALESCE(EXCLUDED.push_token, privchat_user_devices.push_token),
                apns_armed = EXCLUDED.apns_armed,
                -- 老客户端不报 locale，别把之前存好的值抹成 NULL。
                locale = COALESCE(EXCLUDED.locale, privchat_user_devices.locale),
                -- push_sound 同理：不报就保持原值，而不是被 default 顶回 true。
                push_sound = COALESCE($8, privchat_user_devices.push_sound),
                updated_at = NOW()
            "#,
        )
        .bind(user_id as i64)
        .bind(device_id)
        .bind(&platform)
        .bind(inferred_vendor.as_str())
        .bind(token)
        .bind(apns_armed)
        .bind(locale.map(str::trim).filter(|it| !it.is_empty()))
        .bind(push_sound)
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

#[cfg(test)]
mod push_preference_tests {
    use super::*;
    use sqlx::postgres::PgPoolOptions;

    async fn pool() -> Option<PgPool> {
        let url = crate::require_test_database_url()?;
        Some(
            PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await
                .expect("连接测试库"),
        )
    }

    /// 清掉偏好，并保证用户行存在（`privchat_user_settings.user_id` 有外键）。
    async fn reset(pool: &PgPool, user_id: i64) {
        sqlx::query("DELETE FROM privchat_user_settings WHERE user_id = $1 AND setting_key = $2")
            .bind(user_id)
            .bind(PUSH_PREFERENCE_KEY)
            .execute(pool)
            .await
            .expect("清理偏好");
        sqlx::query(
            r#"
            INSERT INTO privchat_users (user_id, username, display_name, qr_key)
            VALUES ($1, $2, $2, $3)
            ON CONFLICT (user_id) DO NOTHING
            "#,
        )
        .bind(user_id)
        .bind(format!("pushpref{user_id}"))
        .bind(format!("qk{user_id}"))
        .execute(pool)
        .await
        .expect("预置用户行");
    }

    /// 没有记录 = 用户没设置过，用产品默认值（显示预览、不静音）。
    #[tokio::test]
    async fn missing_record_falls_back_to_product_defaults() {
        let Some(pool) = pool().await else { return };
        let uid = 990_001_i64;
        reset(&pool, uid).await;

        let repo = UserDeviceRepository::new(pool.clone());
        let preference = repo.get_push_preference(uid as u64).await.expect("读取偏好");
        assert!(preference.show_preview);
        assert!(!preference.global_mute);
    }

    /// 🔴 两台设备同时改**不同**字段，两项改动都要保留。
    ///
    /// 先 SELECT 再整体 UPDATE 的实现会在这里挂：两个请求都读到初始值，
    /// 后写的那个把对方刚改的字段写回了旧值。
    #[tokio::test]
    async fn concurrent_updates_to_different_fields_both_survive() {
        let Some(pool) = pool().await else { return };
        let uid = 990_002_i64;
        reset(&pool, uid).await;

        let repo = std::sync::Arc::new(UserDeviceRepository::new(pool.clone()));
        // 先落一条初始记录，让两个并发请求都走 ON CONFLICT 分支（真正的竞态点）。
        repo.merge_push_preference(uid as u64, Some(true), Some(false))
            .await
            .expect("初始化偏好");

        let a = {
            let repo = repo.clone();
            tokio::spawn(async move { repo.merge_push_preference(uid as u64, Some(false), None).await })
        };
        let b = {
            let repo = repo.clone();
            tokio::spawn(async move { repo.merge_push_preference(uid as u64, None, Some(true)).await })
        };
        a.await.expect("join a").expect("设备 A 更新");
        b.await.expect("join b").expect("设备 B 更新");

        let final_state = repo.get_push_preference(uid as u64).await.expect("读取偏好");
        assert!(
            !final_state.show_preview,
            "设备 A 关闭预览的改动被覆盖了: {:?}",
            final_state
        );
        assert!(
            final_state.global_mute,
            "设备 B 开启免打扰的改动被覆盖了: {:?}",
            final_state
        );
    }

    /// 只提供一个字段时，另一个字段保持库里的原值——不能被默认值顶掉。
    #[tokio::test]
    async fn partial_update_keeps_the_other_field() {
        let Some(pool) = pool().await else { return };
        let uid = 990_003_i64;
        reset(&pool, uid).await;

        let repo = UserDeviceRepository::new(pool.clone());
        repo.merge_push_preference(uid as u64, Some(false), Some(true))
            .await
            .expect("初始化偏好");

        // 只改 global_mute，show_preview 必须还是 false（而不是回到默认的 true）。
        let merged = repo
            .merge_push_preference(uid as u64, None, Some(false))
            .await
            .expect("部分更新");
        assert!(!merged.show_preview);
        assert!(!merged.global_mute);
    }

    /// 🔴 读不到偏好时必须报错，不能返回"显示预览、不静音"的默认值：
    /// 那会让数据库抖动直接翻译成隐私泄露 + 免打扰失效。
    #[tokio::test]
    async fn load_push_context_fails_loudly_when_query_fails() {
        let Some(url) = crate::require_test_database_url() else { return };
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("连接测试库");
        // 关掉连接池再查：模拟数据库不可达。
        pool.close().await;

        let repo = UserDeviceRepository::new(pool);
        let result = repo.load_push_context(990_004, 1).await;
        assert!(
            result.is_err(),
            "查询失败时返回了默认值，这会把正文推给关掉预览的用户"
        );
    }
}
