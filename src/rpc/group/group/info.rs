use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::RpcServiceContext;
use privchat_protocol::rpc::group::group::GroupInfoRequest;
use serde_json::{json, Value};

/// 处理 群组信息 请求
pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    // ✨ 使用协议层类型自动反序列化
    let mut request: GroupInfoRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {}", e)))?;

    // 从 ctx 填充 user_id
    request.user_id = crate::rpc::get_current_user_id(&ctx)?;

    let group_id = request.group_id;

    tracing::debug!("🔧 查询群组信息: {}", group_id);

    // 获取群组信息
    match services.channel_service.get_channel(&group_id).await {
        Ok(channel) => {
            // 获取成员列表
            let members = match services
                .channel_service
                .get_channel_members(&group_id)
                .await
            {
                Ok(members) => members,
                Err(_) => Vec::new(),
            };

            // 创建默认统计信息（get_channel_stats 方法不存在，使用默认值）
            let stats = crate::model::channel::ChannelStats {
                channel_id: group_id,
                member_count: members.len() as u32,
                message_count: channel.message_count as u64,
                today_message_count: 0,
                active_member_count: 0,
                stats_time: chrono::Utc::now(),
            };

            Ok(json!({
                "status": "success",
                "group_info": {
                    "group_id": channel.id,
                    "name": channel.metadata.name,
                    "description": channel.metadata.description,
                    "avatar_url": channel.metadata.avatar_url,
                    "owner_id": channel.creator_id,
                    "created_at": channel.created_at.to_rfc3339(),
                    "updated_at": channel.updated_at.to_rfc3339(),
                    "member_count": stats.member_count,
                    "message_count": stats.message_count,
                    "is_archived": matches!(channel.status, crate::model::channel::ChannelStatus::Archived),
                    "tags": channel.metadata.tags,
                    "custom_fields": channel.metadata.custom_properties
                },
                "members": members.iter().map(|member| json!({
                    "user_id": member.user_id,
                    "role": format!("{:?}", member.role),
                    "joined_at": member.joined_at.to_rfc3339(),
                    "last_active": member.last_active_at.to_rfc3339(),
                    "is_muted": member.is_muted,
                    "display_name": member.display_name,
                })).collect::<Vec<_>>(),
                "timestamp": chrono::Utc::now().to_rfc3339()
            }))
        }
        Err(e) => {
            tracing::error!("❌ 查询群组信息失败: {}", e);
            Err(RpcError::not_found(format!("群组不存在: {}", group_id)))
        }
    }
}
