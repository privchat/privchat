// =====================================================
// 设备管理服务（数据库版本，支持会话管理）
// =====================================================

use crate::auth::models::{Device, DeviceItem, DeviceType};
use crate::auth::{KickedDevice, SessionState, SessionVerifyResult};
use crate::error::{Result, ServerError};
use chrono::Utc;
use sqlx::PgPool;
use std::sync::Arc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// 设备管理服务（数据库版本）
pub struct DeviceManagerDb {
    db_pool: Arc<PgPool>,
}

impl DeviceManagerDb {
    /// 创建新的设备管理器
    pub fn new(db_pool: Arc<PgPool>) -> Self {
        Self { db_pool }
    }

    /// 获取数据库连接池（用于管理 API）
    pub fn pool(&self) -> &PgPool {
        &self.db_pool
    }

    // =====================================================
    // 核心会话管理方法
    // =====================================================

    /// 验证设备会话（核心验证逻辑）
    ///
    /// 检查：
    /// 1. 设备是否存在
    /// 2. 会话状态是否可用
    /// 3. Token 版本是否匹配（使用 < 比较）
    pub async fn verify_device_session(
        &self,
        user_id: u64,
        device_id: &str,
        token_session_version: i64,
    ) -> Result<SessionVerifyResult> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        // 查询设备状态和版本
        let device = sqlx::query!(
            r#"
            SELECT 
                session_version,
                session_state as "session_state: i16"
            FROM privchat_devices
            WHERE user_id = $1 AND device_id = $2
            "#,
            user_id as i64,
            device_uuid,
        )
        .fetch_optional(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备失败: {}", e)))?;

        let device = match device {
            Some(d) => d,
            None => {
                debug!("设备不存在: user={}, device={}", user_id, device_id);
                return Ok(SessionVerifyResult::DeviceNotFound);
            }
        };

        // 转换会话状态
        let session_state: SessionState = unsafe { std::mem::transmute(device.session_state) };

        // 检查会话状态
        if !session_state.is_usable() {
            warn!(
                "设备会话状态不可用: user={}, device={}, state={:?}",
                user_id, device_id, session_state
            );
            return Ok(SessionVerifyResult::SessionInactive {
                state: session_state,
                message: session_state.to_error_message().to_string(),
            });
        }

        // 检查会话版本（注意：是 < 而不是 !=）
        if token_session_version < device.session_version {
            warn!(
                "Token 版本过期: user={}, device={}, token_v={}, current_v={}",
                user_id, device_id, token_session_version, device.session_version
            );
            return Ok(SessionVerifyResult::VersionMismatch {
                token_version: token_session_version,
                current_version: device.session_version,
            });
        }

        // ✅ 验证通过
        debug!(
            "✅ 设备会话验证通过: user={}, device={}, version={}",
            user_id, device_id, device.session_version
        );

        Ok(SessionVerifyResult::Valid {
            session_version: device.session_version,
        })
    }

    /// 踢出其他设备（核心方法）
    ///
    /// 流程：
    /// 1. 查询要踢出的设备
    /// 2. 批量更新：session_version + 1, session_state = KICKED
    /// 3. 返回被踢设备列表（由调用方断开连接）
    pub async fn kick_other_devices(
        &self,
        user_id: u64,
        current_device_id: &str,
        reason: &str,
    ) -> Result<Vec<KickedDevice>> {
        let current_device_uuid = Uuid::parse_str(current_device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        info!(
            "准备踢出其他设备: user={}, current_device={}",
            user_id, current_device_id
        );

        // 1. 查询要踢出的设备
        let devices_to_kick = sqlx::query!(
            r#"
            SELECT 
                device_id,
                device_name,
                device_type,
                session_version
            FROM privchat_devices
            WHERE user_id = $1 
              AND device_id != $2
              AND session_state = 0
            "#,
            user_id as i64,
            current_device_uuid,
        )
        .fetch_all(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备失败: {}", e)))?;

        if devices_to_kick.is_empty() {
            info!("没有其他活跃设备需要踢出: user={}", user_id);
            return Ok(Vec::new());
        }

        info!(
            "找到 {} 个设备需要踢出: user={}",
            devices_to_kick.len(),
            user_id
        );

        // 2. 批量更新设备状态
        let now = chrono::Utc::now().timestamp_millis();
        let kicked_device_ids: Vec<Uuid> = devices_to_kick.iter().map(|d| d.device_id).collect();

        let updated = sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET 
                session_version = session_version + 1,
                session_state = 1,
                kicked_at = $1,
                kicked_by_device_id = $2,
                kicked_reason = $3,
                updated_at = $1
            WHERE user_id = $4
              AND device_id = ANY($5)
              AND session_state = 0
            "#,
            now,
            current_device_uuid,
            reason,
            user_id as i64,
            &kicked_device_ids,
        )
        .execute(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("更新设备状态失败: {}", e)))?;

        info!(
            "✅ 已更新 {} 个设备状态: user={}",
            updated.rows_affected(),
            user_id
        );

        // 3. 构造返回列表
        let kicked_list: Vec<KickedDevice> = devices_to_kick
            .into_iter()
            .map(|d| KickedDevice {
                device_id: d.device_id.to_string(),
                device_name: d.device_name,
                device_type: d.device_type,
                old_version: d.session_version,
                new_version: d.session_version + 1,
            })
            .collect();

        Ok(kicked_list)
    }

    /// 踢出指定设备
    pub async fn kick_device(
        &self,
        user_id: u64,
        target_device_id: &str,
        kicked_by_device_id: Option<&str>,
        reason: &str,
    ) -> Result<()> {
        let target_uuid = Uuid::parse_str(target_device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let kicked_by_uuid = kicked_by_device_id
            .map(|id| Uuid::parse_str(id))
            .transpose()
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let now = chrono::Utc::now().timestamp_millis();

        let result = sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET 
                session_version = session_version + 1,
                session_state = 1,
                kicked_at = $1,
                kicked_by_device_id = $2,
                kicked_reason = $3,
                updated_at = $1
            WHERE user_id = $4
              AND device_id = $5
              AND session_state = 0
            RETURNING device_id
            "#,
            now,
            kicked_by_uuid,
            reason,
            user_id as i64,
            target_uuid,
        )
        .fetch_optional(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("踢出设备失败: {}", e)))?;

        match result {
            Some(_) => {
                info!(
                    "✅ 设备已踢出: user={}, device={}, reason={}",
                    user_id, target_device_id, reason
                );
                Ok(())
            }
            None => Err(ServerError::NotFound("设备不存在或已被踢出".to_string())),
        }
    }

    /// 递增设备会话版本（用于安全事件）
    pub async fn increment_session_version(
        &self,
        user_id: u64,
        device_id: &str,
        reason: &str,
    ) -> Result<i64> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let now = chrono::Utc::now().timestamp_millis();

        let result = sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET 
                session_version = session_version + 1,
                kicked_reason = $1,
                updated_at = $2
            WHERE user_id = $3 AND device_id = $4
            RETURNING session_version
            "#,
            reason,
            now,
            user_id as i64,
            device_uuid,
        )
        .fetch_one(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("递增版本失败: {}", e)))?;

        info!(
            "✅ 会话版本已递增: user={}, device={}, new_version={}, reason={}",
            user_id, device_id, result.session_version, reason
        );

        Ok(result.session_version)
    }

    /// 撤销所有设备（用于密码修改、账号注销等）
    pub async fn revoke_all_devices(&self, user_id: u64, reason: &str) -> Result<usize> {
        let now = chrono::Utc::now().timestamp_millis();

        let result = sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET 
                session_version = session_version + 1,
                session_state = 3,
                kicked_at = $1,
                kicked_reason = $2,
                updated_at = $1
            WHERE user_id = $3 AND session_state = 0
            "#,
            now,
            reason,
            user_id as i64,
        )
        .execute(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("撤销设备失败: {}", e)))?;

        let count = result.rows_affected() as usize;

        info!(
            "✅ 所有设备已撤销: user={}, count={}, reason={}",
            user_id, count, reason
        );

        Ok(count)
    }

    /// 重新激活设备（被踢后重新登录）
    pub async fn reactivate_device(&self, user_id: u64, device_id: &str) -> Result<()> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET 
                session_state = 0,
                kicked_at = NULL,
                kicked_by_device_id = NULL,
                kicked_reason = NULL,
                updated_at = $1
            WHERE user_id = $2 AND device_id = $3
            "#,
            now,
            user_id as i64,
            device_uuid,
        )
        .execute(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("重新激活设备失败: {}", e)))?;

        info!("✅ 设备已重新激活: user={}, device={}", user_id, device_id);

        Ok(())
    }

    // =====================================================
    // 基础设备管理方法
    // =====================================================

    /// 注册新设备或更新现有设备
    pub async fn register_or_update_device(&self, device: &Device) -> Result<()> {
        let device_uuid = Uuid::parse_str(&device.device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let now = chrono::Utc::now();

        sqlx::query!(
            r#"
            INSERT INTO privchat_devices (
                device_id, user_id, device_type, device_name, device_model,
                os_version, app_version, session_version, session_state,
                last_active_at, last_ip, created_at
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, 1, 0, $8, $9, $8)
            ON CONFLICT (user_id, device_id) 
            DO UPDATE SET
                device_name = EXCLUDED.device_name,
                device_model = EXCLUDED.device_model,
                os_version = EXCLUDED.os_version,
                app_version = EXCLUDED.app_version,
                last_active_at = EXCLUDED.last_active_at,
                last_ip = EXCLUDED.last_ip
            "#,
            device_uuid,
            device.user_id as i64,
            device.device_type.as_str(),
            device.device_info.device_name,
            device.device_info.device_model,
            device.device_info.os_version,
            device.device_info.app_version,
            now.timestamp_millis(),
            device.ip_address,
        )
        .execute(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("注册设备失败: {}", e)))?;

        info!("✅ 设备已注册/更新: device_id={}", device.device_id);

        Ok(())
    }

    /// 获取设备信息和会话版本
    pub async fn get_device_with_version(
        &self,
        user_id: u64,
        device_id: &str,
    ) -> Result<Option<(String, i64)>> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let result = sqlx::query!(
            r#"
            SELECT device_id, session_version
            FROM privchat_devices
            WHERE user_id = $1 AND device_id = $2
            "#,
            user_id as i64,
            device_uuid,
        )
        .fetch_optional(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备失败: {}", e)))?;

        Ok(result.map(|r| (r.device_id.to_string(), r.session_version)))
    }

    /// 获取用户的所有设备
    pub async fn get_user_devices(&self, user_id: u64) -> Result<Vec<DeviceItem>> {
        let devices = sqlx::query!(
            r#"
            SELECT 
                device_id,
                device_name,
                device_model,
                device_type,
                app_version,
                session_state as "session_state: i16",
                last_active_at,
                last_ip as ip_address,
                created_at
            FROM privchat_devices
            WHERE user_id = $1
            ORDER BY last_active_at DESC
            "#,
            user_id as i64,
        )
        .fetch_all(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备列表失败: {}", e)))?;

        let items: Vec<DeviceItem> = devices
            .into_iter()
            .map(|d| {
                let device_type: DeviceType = DeviceType::from_str(&d.device_type);
                DeviceItem {
                    device_id: d.device_id.to_string(),
                    device_name: d.device_name.unwrap_or_default(),
                    device_model: d.device_model.unwrap_or_default(),
                    app_id: d.app_version.unwrap_or_default(),
                    device_type,
                    last_active_at: chrono::DateTime::from_timestamp_millis(
                        d.last_active_at.unwrap_or(0),
                    )
                    .unwrap_or_else(|| Utc::now()),
                    created_at: chrono::DateTime::from_timestamp_millis(d.created_at)
                        .unwrap_or_else(|| Utc::now()),
                    ip_address: d.ip_address.unwrap_or_default(),
                    is_current: false,
                }
            })
            .collect();

        Ok(items)
    }

    /// 更新设备最后活跃时间
    pub async fn update_last_active(&self, user_id: u64, device_id: &str, ip: &str) -> Result<()> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let now = chrono::Utc::now().timestamp_millis();

        sqlx::query!(
            r#"
            UPDATE privchat_devices
            SET last_active_at = $1, last_ip = $2
            WHERE user_id = $3 AND device_id = $4
            "#,
            now,
            ip,
            user_id as i64,
            device_uuid,
        )
        .execute(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("更新活跃时间失败: {}", e)))?;

        debug!("更新设备活跃时间: device_id={}", device_id);

        Ok(())
    }

    // =====================================================
    // 管理 API 方法
    // =====================================================

    /// 获取设备列表（管理 API）
    pub async fn list_devices_admin(
        &self,
        page: u32,
        page_size: u32,
        user_id_filter: Option<u64>,
    ) -> Result<(Vec<DeviceItem>, u32)> {
        let offset = (page - 1) * page_size;

        let devices = if let Some(user_id) = user_id_filter {
            // 查询指定用户的设备
            self.get_user_devices(user_id).await?
        } else {
            // 查询所有设备（分页）
            let device_rows = sqlx::query!(
                r#"
            SELECT 
                device_id,
                user_id,
                device_name,
                device_model,
                device_type,
                app_version,
                session_state as "session_state: i16",
                last_active_at,
                last_ip as ip_address,
                created_at
            FROM privchat_devices
            ORDER BY last_active_at DESC
            LIMIT $1 OFFSET $2
                "#,
                page_size as i64,
                offset as i64,
            )
            .fetch_all(&*self.db_pool)
            .await
            .map_err(|e| ServerError::Database(format!("查询设备列表失败: {}", e)))?;

            device_rows
                .into_iter()
                .map(|d| {
                    let device_type = DeviceType::from_str(&d.device_type);
                    DeviceItem {
                        device_id: d.device_id.to_string(),
                        device_name: d.device_name.unwrap_or_default(),
                        device_model: d.device_model.unwrap_or_default(),
                        app_id: d.app_version.unwrap_or_default(), // 注意：数据库字段是 app_version
                        device_type,
                        last_active_at: chrono::DateTime::from_timestamp_millis(
                            d.last_active_at.unwrap_or(0),
                        )
                        .unwrap_or_else(|| Utc::now()),
                        created_at: chrono::DateTime::from_timestamp_millis(d.created_at)
                            .unwrap_or_else(|| Utc::now()),
                        ip_address: d.ip_address.unwrap_or_default(),
                        is_current: false,
                    }
                })
                .collect()
        };

        let total = if user_id_filter.is_some() {
            devices.len() as u32
        } else {
            let result = sqlx::query!(
                r#"
                SELECT COUNT(*) as count
                FROM privchat_devices
                "#
            )
            .fetch_one(&*self.db_pool)
            .await
            .map_err(|e| ServerError::Database(format!("统计设备数失败: {}", e)))?;
            result.count.unwrap_or(0) as u32
        };

        Ok((devices, total))
    }

    /// 获取设备详情（管理 API）
    pub async fn get_device_admin(&self, device_id: &str) -> Result<DeviceItem> {
        let device_uuid = Uuid::parse_str(device_id)
            .map_err(|_| ServerError::BadRequest("无效的设备ID".to_string()))?;

        let device = sqlx::query!(
            r#"
            SELECT 
                device_id,
                user_id,
                device_name,
                device_model,
                device_type,
                app_version,
                session_state as "session_state: i16",
                session_version,
                last_active_at,
                last_ip as ip_address,
                created_at
            FROM privchat_devices
            WHERE device_id = $1
            LIMIT 1
            "#,
            device_uuid,
        )
        .fetch_optional(&*self.db_pool)
        .await
        .map_err(|e| ServerError::Database(format!("查询设备详情失败: {}", e)))?;

        let device =
            device.ok_or_else(|| ServerError::NotFound(format!("设备 {} 不存在", device_id)))?;

        let device_type = DeviceType::from_str(&device.device_type);

        Ok(DeviceItem {
            device_id: device.device_id.to_string(),
            device_name: device.device_name.unwrap_or_default(),
            device_model: device.device_model.unwrap_or_default(),
            app_id: device.app_version.unwrap_or_default(), // 注意：数据库字段是 app_version
            device_type,
            last_active_at: chrono::DateTime::from_timestamp_millis(
                device.last_active_at.unwrap_or(0),
            )
            .unwrap_or_else(|| Utc::now()),
            created_at: chrono::DateTime::from_timestamp_millis(device.created_at)
                .unwrap_or_else(|| Utc::now()),
            ip_address: device.ip_address.unwrap_or_default(),
            is_current: false,
        })
    }
}

impl DeviceType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "ios" => Self::IOS,
            "android" => Self::Android,
            "macos" => Self::MacOS,
            "windows" => Self::Windows,
            "linux" => Self::Linux,
            "mobile" => Self::Mobile,
            "desktop" => Self::Desktop,
            "web" => Self::Web,
            _ => Self::Unknown,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::IOS => "ios",
            Self::Android => "android",
            Self::MacOS => "macos",
            Self::Windows => "windows",
            Self::Linux => "linux",
            Self::Mobile => "mobile",
            Self::Desktop => "desktop",
            Self::Web => "web",
            Self::Unknown => "unknown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: 添加集成测试（需要测试数据库）
}
