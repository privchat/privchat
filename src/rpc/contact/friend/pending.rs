use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{helpers, RpcServiceContext};
use serde_json::{json, Value};

/// 处理 待处理好友申请列表 请求
///
/// 返回接收到的待处理好友申请，包含来源信息，方便用户溯源
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    tracing::debug!("🔧 处理待处理好友申请列表请求: {:?}", body);

    // 从 ctx 获取当前用户 ID
    let user_id = crate::rpc::get_current_user_id(&ctx)?;

    // 获取待处理的好友申请
    match services.friend_service.get_pending_requests(user_id).await {
        Ok(requests) => {
            let mut request_list = Vec::new();

            for request in requests {
                // 获取申请者的用户资料
                let mut request_json = json!({
                    "request_id": request.id,
                    "from_user_id": request.from_user_id,
                    "message": request.message,
                    "status": format!("{:?}", request.status),
                    "created_at": request.created_at.to_rfc3339(),
                });

                // 添加来源信息（用于溯源）
                if let Some(source) = &request.source {
                    let (source_type, source_id, source_desc) = match source {
                        crate::model::privacy::FriendRequestSource::Search {
                            search_session_id,
                        } => (
                            "search",
                            search_session_id.to_string(),
                            "通过搜索添加".to_string(),
                        ),
                        crate::model::privacy::FriendRequestSource::Group { group_id } => {
                            // 尝试获取群组名称
                            let group_name =
                                match services.channel_service.get_channel(&group_id).await {
                                    Ok(ch) => {
                                        ch.metadata.name.unwrap_or_else(|| group_id.to_string())
                                    }
                                    Err(_) => group_id.to_string(),
                                };
                            (
                                "group",
                                group_id.to_string(),
                                format!("通过群组「{}」添加", group_name),
                            )
                        }
                        crate::model::privacy::FriendRequestSource::CardShare { share_id } => {
                            // 尝试获取分享者信息（从数据库读取）
                            let sharer_name =
                                match services.cache_manager.get_card_share(*share_id).await {
                                    Ok(Some(share)) => {
                                        match helpers::get_user_profile_with_fallback(
                                            share.sharer_id,
                                            &services.user_repository,
                                            &services.cache_manager,
                                        )
                                        .await
                                        {
                                            Ok(Some(profile)) => profile.nickname,
                                            _ => "未知用户".to_string(),
                                        }
                                    }
                                    _ => "未知用户".to_string(),
                                };
                            (
                                "card_share",
                                share_id.to_string(),
                                format!("通过{}分享的名片添加", sharer_name),
                            )
                        }
                        crate::model::privacy::FriendRequestSource::Qrcode { qrcode: _ } => {
                            ("qrcode", "".to_string(), "通过二维码添加".to_string())
                        }
                        crate::model::privacy::FriendRequestSource::Phone { phone } => {
                            ("phone", phone.clone(), format!("通过手机号 {} 添加", phone))
                        }
                    };

                    request_json["source"] = json!({
                        "type": source_type,
                        "id": source_id,
                        "description": source_desc,
                    });
                } else {
                    request_json["source"] = json!({
                        "type": "unknown",
                        "description": "未知来源",
                    });
                }

                // 获取申请者的详细信息（从数据库读取）
                if let Ok(Some(profile)) = helpers::get_user_profile_with_fallback(
                    request.from_user_id,
                    &services.user_repository,
                    &services.cache_manager,
                )
                .await
                {
                    request_json["from_user"] = json!({
                        "user_id": profile.user_id,
                        "username": profile.username,
                        "nickname": profile.nickname,
                        "avatar_url": profile.avatar_url,
                    });
                }

                request_list.push(request_json);
            }

            tracing::debug!(
                "✅ 获取待处理好友申请列表成功: {} 有 {} 个待处理申请",
                user_id,
                request_list.len()
            );
            Ok(json!({
                "requests": request_list,
                "total": request_list.len(),
            }))
        }
        Err(e) => {
            tracing::error!("❌ 获取待处理好友申请列表失败: {}", e);
            Err(RpcError::internal(format!(
                "获取待处理好友申请列表失败: {}",
                e
            )))
        }
    }
}
