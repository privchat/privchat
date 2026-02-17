use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

#[derive(Debug, sqlx::FromRow)]
struct DeviceRow {
    device_id: uuid::Uuid,
    device_type: String,
    device_name: Option<String>,
    device_model: Option<String>,
    os_version: Option<String>,
    app_id: String,
    app_version: Option<String>,
    session_version: i64,
    session_state: i16,
    last_active_at: Option<i64>,
    last_ip: Option<String>,
    created_at: i64,
}

fn device_type_label(raw: &str) -> &'static str {
    match raw.to_lowercase().as_str() {
        "ios" => "iOS",
        "android" => "Android",
        "windows" => "Windows",
        "macos" => "macOS",
        "web" => "Web",
        "linux" => "Linux",
        "desktop" => "Desktop",
        "mobile" => "Mobile",
        _ => "Unknown",
    }
}

fn app_display_name(app_id: &str, device_type: &str, app_version: Option<&str>) -> String {
    let base = if app_id.contains("android") {
        "PrivChat Android"
    } else if app_id.contains("ios") {
        "PrivChat iOS"
    } else if app_id.contains("windows") {
        "PrivChat Windows"
    } else if app_id.contains("macos") || app_id.contains("mac") {
        "PrivChat macOS"
    } else if app_id.contains("web") {
        "PrivChat Web"
    } else {
        match device_type_label(device_type) {
            "Android" => "PrivChat Android",
            "iOS" => "PrivChat iOS",
            "Windows" => "PrivChat Windows",
            "macOS" => "PrivChat macOS",
            "Web" => "PrivChat Web",
            _ => "PrivChat",
        }
    };
    match app_version {
        Some(v) if !v.trim().is_empty() => format!("{} {}", base, v.trim()),
        _ => base.to_string(),
    }
}

fn location_label(last_ip: Option<&str>) -> String {
    match last_ip {
        Some(ip) if !ip.trim().is_empty() => {
            // 当前未接入 IP 地理库，先返回占位，避免前端空值。
            if ip == "127.0.0.1" || ip == "::1" || ip.starts_with("192.168.") || ip.starts_with("10.") {
                "LAN / Local Network".to_string()
            } else {
                "Unknown".to_string()
            }
        }
        _ => "Unknown".to_string(),
    }
}

/// RPC: device/list - 查询用户设备列表（含显性+隐性字段）
pub async fn handle(ctx: &RpcServiceContext, params: Value) -> RpcResult<Value> {
    tracing::debug!("RPC device/list 请求: {:?}", params);

    let user_id = params
        .get("user_id")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| RpcError::validation("缺少 user_id 参数".to_string()))?;
    let current_device_id = params.get("device_id").and_then(|v| v.as_str());

    let rows = sqlx::query_as::<_, DeviceRow>(
        r#"
        SELECT
            device_id,
            device_type,
            device_name,
            device_model,
            os_version,
            app_id,
            app_version,
            session_version,
            session_state,
            last_active_at,
            last_ip,
            created_at
        FROM privchat_devices
        WHERE user_id = $1
        ORDER BY last_active_at DESC NULLS LAST, created_at DESC
        "#,
    )
    .bind(user_id as i64)
    .fetch_all(ctx.device_manager_db.pool())
    .await
    .map_err(|e| RpcError::internal(format!("查询设备列表失败: {}", e)))?;

    let mut devices = Vec::with_capacity(rows.len());
    for row in rows {
        let device_id = row.device_id.to_string();
        let online = ctx
            .connection_manager
            .is_device_online(user_id, &device_id)
            .await;

        let d_type = device_type_label(&row.device_type).to_string();
        let app_name = app_display_name(&row.app_id, &row.device_type, row.app_version.as_deref());
        let location = location_label(row.last_ip.as_deref());
        let last_active_at_ms = row.last_active_at.unwrap_or(0);
        let last_active_at = chrono::DateTime::from_timestamp_millis(last_active_at_ms)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        let created_at = chrono::DateTime::from_timestamp_millis(row.created_at)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();

        devices.push(json!({
            // 显性字段（前端可直接展示）
            "device_id": device_id,
            "device_type": d_type,
            "device_name": row.device_model.clone().unwrap_or_else(|| row.device_name.clone().unwrap_or_else(|| "Unknown Device".to_string())),
            "app_name": app_name,
            "location": location,
            "online_status": if online { "Online" } else { "Offline" },
            "last_active_at": last_active_at,
            "is_current": current_device_id.map(|id| id == row.device_id.to_string()).unwrap_or(false),

            // 隐性字段（用于后续策略和管理）
            "meta": {
                "device_name_custom": row.device_name,
                "device_model": row.device_model,
                "device_type_raw": row.device_type,
                "app_id": row.app_id,
                "app_version": row.app_version,
                "os_version": row.os_version,
                "session_state": row.session_state,
                "session_version": row.session_version,
                "last_ip": row.last_ip,
                "last_active_at_ms": last_active_at_ms,
                "created_at": created_at,
                "created_at_ms": row.created_at
            }
        }));
    }

    Ok(json!({
        "devices": devices,
        "total": devices.len()
    }))
}
