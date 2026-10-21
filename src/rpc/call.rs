//! Call RPC wiring and durable post-commit effects. No chat subscription is required.
use super::{
    error::{RpcError, RpcResult},
    get_current_user_id,
    router::GLOBAL_RPC_ROUTER,
    RpcContext, RpcServiceContext,
};
use crate::service::{call_service::CallService, message_service::ServerSendMessageRequest};
use privchat_protocol::{
    protocol::{MessageType, PublishRequest},
    rpc::{call::*, routes::call as routes},
};
use serde_json::{json, Value};
use sqlx::Row;
use std::sync::Arc;

pub async fn register_routes(services: RpcServiceContext) {
    let service = match CallService::from_environment(
        services.message_repository.pool().clone(),
        services.config.call.clone(),
    ) {
        Ok(service) => service,
        Err(_) => {
            tracing::error!(
                "Call configuration invalid or credentials missing; call capability disabled"
            );
            CallService::new(
                services.message_repository.pool().clone(),
                Default::default(),
                None,
            )
            .expect("disabled calls")
        }
    };
    let service = Arc::new(service);
    for route in [
        routes::CAPABILITIES,
        routes::INVITE,
        routes::JOIN,
        routes::REJECT,
        routes::CANCEL,
        routes::LEAVE,
        routes::END,
        routes::GET,
        routes::ACTIVE,
        routes::HEARTBEAT,
    ] {
        let service = service.clone();
        GLOBAL_RPC_ROUTER
            .register(route, move |params, ctx| {
                let service = service.clone();
                async move { handle(&service, route, params, ctx).await }
            })
            .await;
    }
    if service.config.enabled {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            loop {
                interval.tick().await;
                if service.sweep().await.is_err() {
                    tracing::warn!("Call lease sweep failed; will retry");
                }
                for _ in 0..50 {
                    match dispatch_one(&service, &services).await {
                        Ok(true) => (),
                        Ok(false) => break,
                        Err(_) => {
                            tracing::warn!("Call effect failed; durable retry scheduled");
                            break;
                        }
                    }
                }
            }
        });
    }
}
async fn handle(
    service: &CallService,
    route: &str,
    params: Value,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let actor = get_current_user_id(&ctx)?;
    let device = ctx
        .device_id
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RpcError::unauthorized("Authenticated device required"))?;
    let invalid = |_| RpcError::validation("Invalid call request");
    match route {
        routes::CAPABILITIES => Ok(json!(service.capabilities())),
        routes::INVITE => {
            service
                .invite(
                    actor,
                    device,
                    serde_json::from_value(params).map_err(invalid)?,
                )
                .await
        }
        routes::GET => {
            let req: CallGetRequest = serde_json::from_value(params).map_err(invalid)?;
            Ok(json!(CallResponse {
                call: service.get(actor, req.call_id).await?
            }))
        }
        routes::ACTIVE => Ok(json!(service.active(actor).await?)),
        _ => {
            service
                .mutate(
                    route.trim_start_matches("call/"),
                    actor,
                    device,
                    serde_json::from_value(params).map_err(invalid)?,
                )
                .await
        }
    }
}

async fn dispatch_one(service: &CallService, services: &RpcServiceContext) -> anyhow::Result<bool> {
    // Lock the work item across bounded effects. Crash before completion causes replay;
    // history writes have a permanent dedup key, state consumers use call.version.
    let mut tx = service.pool.begin().await?;
    let row = sqlx::query("SELECT id,call_id,kind,payload FROM privchat_call_effects WHERE NOT completed AND next_attempt_at <= $1 ORDER BY id FOR UPDATE SKIP LOCKED LIMIT 1")
        .bind(chrono::Utc::now().timestamp_millis()).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let id: i64 = row.get("id");
    let call_id: i64 = row.get("call_id");
    let kind: String = row.get("kind");
    let payload: Value = row.get("payload");
    let stored =
        sqlx::query("SELECT document,history_message_id FROM privchat_calls WHERE call_id=$1")
            .bind(call_id)
            .fetch_one(&mut *tx)
            .await?;
    let document: Value = stored.get("document");
    let call: CallSnapshot = serde_json::from_value(document["call"].clone())?;
    let result: anyhow::Result<()> = async {
        match kind.as_str() {
            "state" => {
                let event = PublishRequest {
                    channel_id:call.channel_id, topic:Some(routes::STATE.into()),timestamp:chrono::Utc::now().timestamp_millis() as u64,
                    payload:serde_json::to_vec(&CallResponse {call:call.clone()})?,publisher:None,server_message_id:None,
                };
                let bytes = privchat_protocol::encode_message(&event)?;
                for participant in &call.participants {
                    // Recheck channel membership before disclosing the snapshot.
                    if visible_state_recipient(service.get(participant.user_id,call.call_id).await)?.is_none() { continue; }
                    for connection in services.connection_manager.get_user_connections(participant.user_id).await {
                        let transport = services.connection_manager.transport_server.read().await;
                        let Some(server) = transport.as_ref() else { anyhow::bail!("Call transport unavailable"); };
                        tokio::time::timeout(std::time::Duration::from_secs(5), server.send_with_options(connection.session_id,bytes.clone().into(),msgtrans::SendOptions::new().biz_type(MessageType::PublishRequest as u8))).await??;
                    }
                }
            }
            "history" => {
                if stored.get::<Option<i64>,_>("history_message_id").is_none() {
                    let content = json!({"event":"call.ended","call_id":call.call_id,"media_kind":call.media_kind,
                        "duration_ms":call.answered_at_ms.zip(call.ended_at_ms).map(|(a,b)|(b-a).max(0)).unwrap_or(0),"end_reason":call.end_reason});
                    let result = services.message_service.send_message(ServerSendMessageRequest {
                        channel_id:call.channel_id,sender_id:crate::config::SYSTEM_USER_ID,content:content.to_string(),
                        message_type:privchat_protocol::ContentMessageType::System, metadata:content,
                        channel_type:document["channel_type"].as_u64().unwrap_or(0) as u8 + 1,
                        recipient_user_ids:call.participants.iter().map(|p|p.user_id).collect(),
                        dedup_key:Some(format!("call-summary:{}",call.call_id)),attachment_refs_override:None,
                    }).await?;
                    sqlx::query("UPDATE privchat_calls SET history_message_id=$2 WHERE call_id=$1").bind(call_id).bind(result.message_id as i64).execute(&mut *tx).await?;
                }
            }
            "delete_room" => service.media_cleanup(&call,None).await?,
            "remove_participant" => service.media_cleanup(&call,Some(payload["identity"].as_str().ok_or_else(||anyhow::anyhow!("Missing media identity"))?)).await?,
            _ => anyhow::bail!("Unknown call effect"),
        }
        Ok(())
    }.await;
    sqlx::query("UPDATE privchat_call_effects SET completed=$2,next_attempt_at=$3 WHERE id=$1")
        .bind(id)
        .bind(result.is_ok())
        .bind(chrono::Utc::now().timestamp_millis() + 5000)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    result?;
    Ok(true)
}

// Only an explicit authorization refusal is a permanent skip. Storage, decoding,
// and missing-state failures must reach the effect runner's durable retry branch.
fn visible_state_recipient(
    result: RpcResult<CallSnapshot>,
) -> anyhow::Result<Option<CallSnapshot>> {
    match result {
        Ok(call) => Ok(Some(call)),
        Err(error) if error.code == privchat_protocol::ErrorCode::PermissionDenied => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_visibility_read_failure_retries_instead_of_completing_effect() {
        for code in [
            privchat_protocol::ErrorCode::ServiceUnavailable,
            privchat_protocol::ErrorCode::InternalError,
            privchat_protocol::ErrorCode::ResourceNotFound,
        ] {
            let effect_result = visible_state_recipient(Err(RpcError::from_code(
                code,
                "injected visibility read failure".into(),
            )));
            assert!(
                !effect_result.is_ok(),
                "effect completion must remain false on read failure"
            );
        }
        assert!(
            visible_state_recipient(Err(RpcError::forbidden("no longer a member")))
                .unwrap()
                .is_none()
        );
    }
}
