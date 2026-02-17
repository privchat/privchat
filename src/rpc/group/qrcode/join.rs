use crate::model::QRType;
use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use privchat_protocol::rpc::GroupQRCodeJoinRequest;
use serde_json::{json, Value};

/// 处理 扫码加群 请求
///
/// RPC: group/join/qrcode
///
/// 使用新的 QR Key 系统，通过 qrkey 和 token 加入群组
///
/// 请求参数：
/// ```json
/// {
///   "user_id": "alice",
///   "qr_code": "privchat://group/get?qrkey=3f4g5h6i7j8k&token=xyz",
///   "message": "我想加入群组"  // 可选：申请理由
/// }
/// ```
///
/// 响应（无需审批）：
/// ```json
/// {
///   "status": "joined",
///   "group_id": "group_123",
///   "message": "已成功加入群组"
/// }
/// ```
///
/// 响应（需要审批）：
/// ```json
/// {
///   "status": "pending",
///   "group_id": "group_123",
///   "request_id": "req_456",
///   "message": "申请已提交，等待管理员审批"
/// }
/// ```
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理 扫码加群 请求: {:?}", body);

    // ✅ 使用 protocol 定义自动反序列化
    let request: GroupQRCodeJoinRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("Invalid request: {}", e)))?;

    // ✅ 从 ctx 获取 user_id（安全性）
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    let qr_key = request.qr_key;
    let token = request.token;
    let message = request.message;

    tracing::debug!(
        "📱 扫描二维码: qr_key={}, has_token={}",
        qr_key,
        token.is_some()
    );

    // 1. 解析 QR Key，验证有效性
    let record = services
        .qrcode_service
        .resolve(&qr_key, user_id, token.as_deref())
        .await
        .map_err(|e| match e {
            crate::error::ServerError::NotFound(_) => {
                RpcError::not_found("二维码不存在或已失效".to_string())
            }
            crate::error::ServerError::BadRequest(msg) => {
                RpcError::validation(format!("二维码无效: {}", msg))
            }
            crate::error::ServerError::Unauthorized(msg) => {
                RpcError::unauthorized(format!("二维码验证失败: {}", msg))
            }
            _ => RpcError::internal(format!("解析二维码失败: {}", e)),
        })?;

    // 2. 验证是群组二维码
    if record.qr_type != QRType::Group {
        return Err(RpcError::validation("不是群组二维码".to_string()));
    }

    // 解析 group_id 为 u64
    let group_id = record.target_id.parse::<u64>().map_err(|_| {
        RpcError::validation(format!("Invalid group_id in QR code: {}", record.target_id))
    })?;

    tracing::debug!(
        "✅ 二维码验证成功: qr_key={}, group_id={}",
        qr_key,
        group_id
    );

    // 3. 验证用户是否存在（从数据库读取）
    let _user_profile = helpers::get_user_profile_with_fallback(
        user_id,
        &services.user_repository,
        &services.cache_manager,
    )
    .await
    .map_err(|e| RpcError::internal(format!("获取用户信息失败: {}", e)))?
    .ok_or_else(|| RpcError::not_found(format!("用户不存在: {}", user_id)))?;

    // 4. 获取群组信息
    let channel = services
        .channel_service
        .get_channel(&group_id)
        .await
        .map_err(|e| RpcError::not_found(format!("群组不存在: {}", e)))?;

    // 5. 检查用户是否已经是群成员
    if channel.members.contains_key(&user_id) {
        return Err(RpcError::validation("您已经是群成员".to_string()));
    }

    // 6. 检查群人数是否已满
    let current_member_count = channel.members.len() as u32;
    let max_members = channel
        .settings
        .as_ref()
        .and_then(|s| s.max_members.map(|m| m as usize))
        .or_else(|| channel.metadata.max_members)
        .unwrap_or(500);
    if current_member_count >= max_members as u32 {
        return Err(RpcError::validation("群组人数已满".to_string()));
    }

    // 7. 检查是否需要审批
    let need_approval = channel
        .settings
        .as_ref()
        .map(|s| s.require_approval)
        .unwrap_or(false);

    if need_approval {
        // 需要审批 - 创建加群申请记录
        tracing::debug!(
            "⏰ 扫码加群需要审批: user_id={}, group_id={}",
            user_id,
            group_id
        );

        // 创建审批请求
        let join_request = services
            .approval_service
            .create_join_request(
                group_id,
                user_id,
                crate::service::JoinMethod::QRCode {
                    qr_code_id: qr_key.clone(),
                },
                message,
                Some(72), // 72小时过期
            )
            .await
            .map_err(|e| RpcError::internal(format!("创建审批请求失败: {}", e)))?;

        // TODO: 通知群管理员有新的加群申请

        Ok(json!({
            "status": "pending",
            "group_id": group_id,
            "request_id": join_request.request_id,
            "message": "申请已提交，等待管理员审批",
            "expires_at": join_request.expires_at.map(|dt| dt.to_rfc3339())
        }))
    } else {
        // 无需审批 - 直接加入群组
        services
            .channel_service
            .join_channel(
                group_id,
                user_id,
                Some(crate::model::channel::MemberRole::Member),
            )
            .await
            .map_err(|e| RpcError::internal(format!("加入群组失败: {}", e)))?;

        tracing::debug!(
            "✅ 扫码加群成功: user_id={}, group_id={}",
            user_id,
            group_id
        );

        Ok(json!({
            "status": "joined",
            "group_id": group_id,
            "message": "已成功加入群组",
            "user_id": user_id,
            "joined_at": chrono::Utc::now().to_rfc3339()
        }))
    }
}
