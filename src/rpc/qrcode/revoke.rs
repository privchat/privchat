use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 撤销 QR 码 请求
///
/// RPC: qrcode/revoke
///
/// 请求参数：
/// ```json
/// {
///   "qr_key": "7a8b9c0d1e2f"
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "success": true,
///   "qr_key": "7a8b9c0d1e2f",
///   "revoked_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 撤销 QR 码 请求: {:?}", body);

    // 解析参数
    let qr_key = body
        .get("qr_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("qr_key is required".to_string()))?;

    // 撤销 QR Key
    services
        .qrcode_service
        .revoke(qr_key)
        .await
        .map_err(|e| match e {
            crate::error::ServerError::NotFound(_) => RpcError::not_found(format!("{}", e)),
            _ => RpcError::internal(format!("撤销 QR 码失败: {}", e)),
        })?;

    tracing::debug!("✅ QR 码撤销成功: qr_key={}", qr_key);

    Ok(json!({
        "success": true,
        "qr_key": qr_key,
        "revoked_at": chrono::Utc::now().to_rfc3339(),
    }))
}
