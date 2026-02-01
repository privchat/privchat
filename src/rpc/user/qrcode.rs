use serde_json::{json, Value};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use crate::model::{QRType, QRKeyOptions};

/// 生成用户名片二维码
/// 
/// RPC: user/qrcode/generate
/// 
/// 请求参数：
/// ```json
/// {
///   "user_id": "alice"
/// }
/// ```
/// 
/// 响应：
/// ```json
/// {
///   "qr_key": "7a8b9c0d1e2f",
///   "qr_code": "privchat://user/get?qrkey=7a8b9c0d1e2f",
///   "created_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn generate(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 生成用户二维码 请求: {:?}", body);
    
    // 从 ctx 获取当前用户 ID
    let user_id_u64 = crate::rpc::get_current_user_id(&ctx)?;
    let user_id = user_id_u64.to_string();
    
    // 生成用户二维码（永久有效，可多次使用）
    let record = services.qrcode_service.generate(
        QRType::User,
        user_id.clone(),
        user_id.clone(),
        QRKeyOptions {
            revoke_old: true,  // 生成新的时撤销旧的
            ..Default::default()
        },
    ).await
        .map_err(|e| RpcError::internal(format!("生成用户二维码失败: {}", e)))?;
    
    tracing::info!(
        "✅ 用户二维码生成成功: user_id={}, qr_key={}, qr_code={}",
        user_id,
        record.qr_key,
        record.to_qr_code_string()
    );
    
    Ok(json!({
        "qr_key": record.qr_key,
        "qr_code": record.to_qr_code_string(),  // privchat://user/get?qrkey=xxx
        "created_at": record.created_at.to_rfc3339(),
    }))
}

/// 刷新用户名片二维码
/// 
/// RPC: user/qrcode/refresh
/// 
/// 请求参数：
/// ```json
/// {
///   "user_id": "alice"
/// }
/// ```
/// 
/// 响应：
/// ```json
/// {
///   "old_qr_key": "7a8b9c0d1e2f",
///   "new_qr_key": "3f4g5h6i7j8k",
///   "new_qr_code": "privchat://user/get?qrkey=3f4g5h6i7j8k",
///   "revoked_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn refresh(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 刷新用户二维码 请求: {:?}", body);
    
    let user_id = body.get("user_id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("user_id is required".to_string()))?;
    
    // 刷新用户二维码
    let (old_qr_key, new_qr_key) = services.qrcode_service.refresh(
        user_id,
        QRType::User,
        user_id,
    ).await
        .map_err(|e| RpcError::internal(format!("刷新用户二维码失败: {}", e)))?;
    
    // 获取新的 QR Key 记录
    let new_record = services.qrcode_service.get(&new_qr_key).await
        .ok_or_else(|| RpcError::internal("无法获取新的 QR Key".to_string()))?;
    
    tracing::info!(
        "✅ 用户二维码刷新成功: user_id={}, old={}, new={}",
        user_id,
        old_qr_key,
        new_qr_key
    );
    
    Ok(json!({
        "old_qr_key": old_qr_key,
        "new_qr_key": new_qr_key,
        "new_qr_code": new_record.to_qr_code_string(),
        "revoked_at": chrono::Utc::now().to_rfc3339(),
    }))
}

/// 获取用户当前的二维码
/// 
/// RPC: user/qrcode/get
/// 
/// 请求参数：
/// ```json
/// {
///   "user_id": "alice"
/// }
/// ```
/// 
/// 响应：
/// ```json
/// {
///   "qr_key": "7a8b9c0d1e2f",
///   "qr_code": "privchat://user/get?qrkey=7a8b9c0d1e2f",
///   "created_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn get(body: Value, services: RpcServiceContext, ctx: crate::rpc::RpcContext) -> RpcResult<Value> {
    tracing::info!("🔧 处理 获取用户二维码 请求: {:?}", body);
    
    let user_id = body.get("user_id").and_then(|v| v.as_str())
        .ok_or_else(|| RpcError::validation("user_id is required".to_string()))?;
    
    // 获取用户当前的二维码
    let record = services.qrcode_service.get_by_target(user_id).await
        .ok_or_else(|| RpcError::not_found("用户二维码不存在".to_string()))?;
    
    // 检查是否有效
    if !record.is_valid() {
        return Err(RpcError::not_found("用户二维码已失效，请重新生成".to_string()));
    }
    
    tracing::info!(
        "✅ 用户二维码获取成功: user_id={}, qr_key={}",
        user_id,
        record.qr_key
    );
    
    Ok(json!({
        "qr_key": record.qr_key,
        "qr_code": record.to_qr_code_string(),
        "created_at": record.created_at.to_rfc3339(),
        "used_count": record.used_count,
    }))
}

