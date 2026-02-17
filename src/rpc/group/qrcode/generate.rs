use crate::model::{QRKeyOptions, QRType};
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::qrcode::utils::generate_random_token;
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::GroupQRCodeGenerateRequest;
use serde_json::{json, Value};

/// 处理 生成群二维码 请求
///
/// RPC: group/qrcode/generate
///
/// 使用新的 QR Key 系统，生成基于 qrkey 的二维码
///
/// 请求参数：
/// ```json
/// {
///   "group_id": "group_123",
///   "operator_id": "alice",          // 操作者ID（需验证权限）
///   "expire_seconds": 604800          // 过期时间（秒），可选，默认7天
/// }
/// ```
///
/// 响应：
/// ```json
/// {
///   "qr_key": "3f4g5h6i7j8k",
///   "qr_code": "privchat://group/get?qrkey=3f4g5h6i7j8k&token=xyz",
///   "expire_at": "2026-01-17T12:00:00Z",
///   "group_id": "group_123",
///   "created_at": "2026-01-10T12:00:00Z"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 生成群二维码 请求: {:?}", body);

    // ✅ 使用 protocol 定义自动反序列化
    let request: GroupQRCodeGenerateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    // ✅ 从 ctx 获取 operator_id（安全性）
    let operator_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;
    let expire_seconds = request.expire_seconds.map(|s| s as i64);

    // 1. 验证群组是否存在
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 2. 验证操作者权限（群主或管理员才能生成二维码）
    let operator_member = channel
        .members
        .get(&operator_id)
        .ok_or_else(|| RpcError::forbidden("您不是群组成员".to_string()))?;

    if !matches!(
        operator_member.role,
        crate::model::channel::MemberRole::Owner | crate::model::channel::MemberRole::Admin
    ) {
        return Err(RpcError::forbidden(
            "只有群主或管理员可以生成群二维码".to_string(),
        ));
    }

    // 3. 生成随机 token（用于额外验证）
    let token = generate_random_token();

    // 4. 使用新的 QR Key 系统生成二维码
    let record = services
        .qrcode_service
        .generate(
            QRType::Group,
            group_id.to_string(),
            operator_id.to_string(),
            QRKeyOptions {
                expire_seconds,
                metadata: json!({ "token": token }),
                revoke_old: true, // 生成新的时撤销旧的
                ..Default::default()
            },
        )
        .await
        .map_err(|e| RpcError::internal(format!("生成二维码失败: {}", e)))?;

    tracing::debug!(
        "✅ 群二维码生成成功: group_id={}, qr_key={}, qr_code={}",
        group_id,
        record.qr_key,
        record.to_qr_code_string()
    );

    Ok(json!({
        "qr_key": record.qr_key,
        "qr_code": record.to_qr_code_string(),  // privchat://group/get?qrkey=xxx&token=yyy
        "expire_at": record.expire_at.map(|t| t.to_rfc3339()),
        "group_id": group_id,
        "created_at": record.created_at.to_rfc3339(),
    }))
}
