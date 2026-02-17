use crate::rpc::error::{RpcError, RpcResult};
use crate::rpc::{get_current_user_id, RpcContext, RpcServiceContext};
use serde_json::{json, Value};

fn parse_session_id(raw: &str) -> RpcResult<msgtrans::SessionId> {
    let id = raw
        .strip_prefix("session-")
        .unwrap_or(raw)
        .parse::<u64>()
        .map_err(|e| RpcError::validation(format!("invalid session_id '{}': {}", raw, e)))?;
    Ok(msgtrans::SessionId::from(id))
}

pub async fn handle(
    _body: Value,
    services: RpcServiceContext,
    ctx: RpcContext,
) -> RpcResult<Value> {
    let user_id = get_current_user_id(&ctx)?;
    let session_id_raw = ctx
        .session_id
        .as_ref()
        .ok_or_else(|| RpcError::unauthorized("missing session_id".to_string()))?;
    let session_id = parse_session_id(session_id_raw)?;

    let removed = services
        .connection_manager
        .unregister_connection(session_id)
        .await
        .map_err(|e| RpcError::internal(format!("unregister connection failed: {}", e)))?;

    services.auth_session_manager.unbind_session(&session_id).await;

    let mut cleanup_device_id = ctx.device_id.clone();
    if cleanup_device_id.is_none() {
        if let Some((removed_uid, removed_device_id)) = removed {
            if removed_uid == user_id {
                cleanup_device_id = Some(removed_device_id);
            }
        }
    }

    if let Some(device_id) = cleanup_device_id {
        let _ = services
            .message_router
            .register_device_offline(&user_id, &device_id, Some(session_id_raw))
            .await;
    }

    Ok(json!({
        "success": true,
        "message": "logout success"
    }))
}
