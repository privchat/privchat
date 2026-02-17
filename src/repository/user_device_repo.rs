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
    ) -> Result<()> {
        if let Some(token) = push_token {
            // 同时更新 apns_armed 和 push_token
            sqlx::query(
                r#"
                UPDATE privchat_user_devices
                SET 
                    apns_armed = $1,
                    push_token = $2,
                    updated_at = NOW()
                WHERE user_id = $3 AND device_id = $4
                "#,
            )
            .bind(apns_armed)
            .bind(token)
            .bind(user_id as i64)
            .bind(device_id)
            .execute(&self.pool)
            .await
            .map_err(|e| ServerError::Database(format!("更新设备推送状态失败: {}", e)))?;
        } else {
            // 只更新 apns_armed
            sqlx::query(
                r#"
                UPDATE privchat_user_devices
                SET 
                    apns_armed = $1,
                    updated_at = NOW()
                WHERE user_id = $2 AND device_id = $3
                "#,
            )
            .bind(apns_armed)
            .bind(user_id as i64)
            .bind(device_id)
            .execute(&self.pool)
            .await
            .map_err(|e| ServerError::Database(format!("更新设备推送状态失败: {}", e)))?;
        }

        Ok(())
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
