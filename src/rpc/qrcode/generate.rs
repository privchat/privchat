use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::model::{QRType, QRKeyOptions};

/// 处理 生成 QR 码 请求
/// 
/// RPC: qrcode/generate
/// 
/// 请求参数：
/// ```json
/// {
///   "qr_type": "user",              // "user" | "group" | "auth" | "feature"
///   "target_id": "alice",           // 目标 ID（用户ID 或 群组ID）
///   "creator_id": "alice",          // 创建者 ID
///   "expire_seconds": 604800,       // 可选：过期时间（秒）
///   "max_usage": 100,               // 可选：最大使用次数
///   "one_time": false,              // 可选：是否一次性
///   "revoke_old": true,             // 可选：是否撤销旧的（默认 true）
///   "metadata": {}                  // 可选：扩展信息
/// }
/// ```
/// 
/// 响应：
/// ```json
/// {
///   "qr_key": "7a8b9c0d1e2f",
///   "qr_code": "privchat://user/get?qrkey=7a8b9c0d1e2f",
///   "qr_type": "user",
///   "target_id": "alice",
///   "created_at": "2026-01-10T12:00:00Z",
///   "expire_at": "2026-01-17T12:00:00Z"
/// }
/// ```
pub async fn handle(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 生成 QR 码 请求: {:?}", body);
    
    // 解析参数
    let qr_type_str = body.get("qr_type").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("qr_type is required".to_string()))?;
    
    let qr_type = QRType::from_str(qr_type_str)
        .ok_or_else(|| RpcError::validation(format!("无效的 qr_type: {}", qr_type_str)))?;
    
    let target_id = body.get("target_id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("target_id is required".to_string()))?;
    
    let creator_id = body.get("creator_id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("creator_id is required".to_string()))?;
    
    // 可选参数
    let expire_seconds = body.get("expire_seconds").and_then(|v| v.as_i64());
    let max_usage = body.get("max_usage").and_then(|v| v.as_i64()).map(|v| v as i32);
    let one_time = body.get("one_time").and_then(|v| v.as_bool()).unwrap_or(false);
    let revoke_old = body.get("revoke_old").and_then(|v| v.as_bool()).unwrap_or(true);
    let metadata = body.get("metadata").cloned().unwrap_or(json!({}));
    
    // 生成选项
    let options = QRKeyOptions {
        expire_seconds,
        max_usage,
        one_time,
        revoke_old,
        metadata,
    };
    
    // 生成 QR Key
    let record = services.qrcode_service.generate(
        qr_type,
        target_id.to_string(),
        creator_id.to_string(),
        options,
    ).await
        .map_err(|e| RpcError::internal(format!("生成 QR 码失败: {}", e)))?;
    
    tracing::info!("✅ QR 码生成成功: qr_key={}, target={}", record.qr_key, target_id);
    
    Ok(json!({
        "qr_key": record.qr_key,
        "qr_code": record.to_qr_code_string(),
        "qr_type": record.qr_type.as_str(),
        "target_id": record.target_id,
        "created_at": record.created_at.to_rfc3339(),
        "expire_at": record.expire_at.map(|t| t.to_rfc3339()),
        "max_usage": record.max_usage,
        "used_count": record.used_count,
    }))
}

