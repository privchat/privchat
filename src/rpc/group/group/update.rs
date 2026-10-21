use crate::rpc::{
    error::{RpcError, RpcResult},
    RpcServiceContext,
};
use privchat_protocol::rpc::group::group::{GroupUpdateRequest, GroupUpdateResponse};
use serde_json::Value;

/// Authorization and mutation share a transaction; never trust cached roles.
pub async fn update_profile(
    pool: &sqlx::PgPool,
    operator: u64,
    mut request: GroupUpdateRequest,
) -> RpcResult<(GroupUpdateResponse, bool)> {
    request
        .profile
        .validate()
        .map_err(|e| RpcError::validation(e.to_string()))?;
    let mut tx = pool.begin().await.map_err(db_error)?;
    let old: (String, Option<String>, i64, i64) = sqlx::query_as(
        "SELECT name, description, updated_at, sync_version FROM privchat_groups \
         WHERE group_id = $1 AND status = 0 FOR UPDATE",
    )
    .bind(request.group_id as i64)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or_else(|| RpcError::not_found("群组不存在"))?;
    let role: Option<i16> = sqlx::query_scalar(
        "SELECT role FROM privchat_group_members WHERE group_id = $1 \
         AND user_id = $2 AND left_at IS NULL FOR UPDATE",
    )
    .bind(request.group_id as i64)
    .bind(operator as i64)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?;
    if !matches!(role, Some(0 | 1)) {
        return Err(RpcError::forbidden("只有群主和管理员可以修改群资料"));
    }
    let name = request.profile.name.unwrap_or_else(|| old.0.clone());
    let description = request.profile.description.or_else(|| old.1.clone());
    let changed = name != old.0 || description != old.1;
    let (updated_at, sync_version) = if changed {
        sqlx::query_as::<_, (i64, i64)>(
            "UPDATE privchat_groups SET name=$2, description=$3, updated_at=now_millis() \
             WHERE group_id=$1 RETURNING updated_at, sync_version",
        )
        .bind(request.group_id as i64)
        .bind(&name)
        .bind(&description)
        .fetch_one(&mut *tx)
        .await
        .map_err(db_error)?
    } else {
        (old.2, old.3)
    };
    tx.commit().await.map_err(db_error)?;
    Ok((
        GroupUpdateResponse {
            group_id: request.group_id,
            name,
            description,
            updated_at: updated_at as u64,
            sync_version: sync_version as u64,
        },
        changed,
    ))
}

fn db_error(error: sqlx::Error) -> RpcError {
    RpcError::internal(format!("群资料操作失败: {error}"))
}

pub async fn handle(
    body: Value,
    services: RpcServiceContext,
    ctx: crate::rpc::RpcContext,
) -> RpcResult<Value> {
    let request: GroupUpdateRequest = serde_json::from_value(body)
        .map_err(|e| RpcError::validation(format!("请求参数格式错误: {e}")))?;
    let operator = crate::rpc::get_current_user_id(&ctx)?;
    let (response, changed) =
        update_profile(services.channel_service.pool(), operator, request).await?;
    if changed {
        services
            .channel_service
            .invalidate_group_profile_cache(response.group_id)
            .await;
        super::notify::publish_changed(&services, response.group_id, "group_profile_changed").await;
        let recipients = sqlx::query_scalar::<_, i64>(
            "SELECT user_id FROM privchat_group_members WHERE group_id=$1 AND left_at IS NULL",
        )
        .bind(response.group_id as i64)
        .fetch_all(services.channel_service.pool())
        .await;
        match recipients {
            Ok(users) => {
                let publisher = crate::service::EntityInvalidationPublisher::new(
                    services.connection_manager.clone(),
                );
                if let Err(error) = publisher
                    .publish_to_users(
                        users.into_iter().map(|id| id as u64),
                        vec![privchat_protocol::EntityInvalidation {
                            entity_type: "group".into(),
                            entity_id: Some(response.group_id.to_string()),
                            scope: None,
                            target_version: response.sync_version,
                            mutation_hint: privchat_protocol::EntityMutationHint::Upsert,
                        }],
                    )
                    .await
                {
                    tracing::warn!(%error, "group profile committed; invalidation failed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "group profile committed; recipient lookup failed")
            }
        }
    }
    serde_json::to_value(response).map_err(|e| RpcError::internal(e.to_string()))
}
