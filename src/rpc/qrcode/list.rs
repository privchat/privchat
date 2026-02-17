use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use serde_json::{json, Value};

/// 处理 列出 QR 码 请求
///
/// RPC: qrcode/list
///
/// 请求参数：
/// ```json
/// {
///   "creator_id": "alice",
///   "include_revoked": false       // 可选：是否包含已撤销的（默认 false）
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "qr_keys": [
///     {
///       "qr_key": "7a8b9c0d1e2f",
///       "qr_type": "user",
///       "target_id": "alice",
///       "created_at": "2026-01-10T12:00:00Z",
///       "expire_at": null,
///       "used_count": 5,
///       "max_usage": null,
///       "revoked": false
///     }
///   ]
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 列出 QR 码 请求: {:?}", body);

    // 解析参数
    let creator_id = body
        .get("creator_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("creator_id is required".to_string()))?;

    let include_revoked = body
        .get("include_revoked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 获取 QR Keys 列表
    let records = services
        .qrcode_service
        .list_by_creator(creator_id, include_revoked)
        .await;

    // 转换为响应格式
    let qr_keys: Vec<Value> = records
        .iter()
        .map(|record| {
            json!({
                "qr_key": record.qr_key,
                "qr_code": record.to_qr_code_string(),
                "qr_type": record.qr_type.as_str(),
                "target_id": record.target_id,
                "created_at": record.created_at.to_rfc3339(),
                "expire_at": record.expire_at.map(|t| t.to_rfc3339()),
                "used_count": record.used_count,
                "max_usage": record.max_usage,
                "revoked": record.revoked,
            })
        })
        .collect();

    tracing::debug!(
        "✅ QR 码列表获取成功: creator={}, count={}",
        creator_id,
        qr_keys.len()
    );

    Ok(json!({
        "qr_keys": qr_keys,
        "total": qr_keys.len(),
    }))
}
