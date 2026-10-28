//! Read-only authenticated component discovery over the existing RPC envelope.
use super::{
    error::{RpcError, RpcResult},
    get_current_user_id, RpcContext,
};
use crate::service::call_service::CallService;
use serde_json::Value;
use std::sync::Arc;

pub async fn register(router: &super::router::RpcRouter, service: Arc<CallService>) {
    router
        .register("config/get", move |_params, ctx| {
            let service = service.clone();
            async move { get(&service, &ctx).await }
        })
        .await;
}

pub async fn get(service: &CallService, ctx: &RpcContext) -> RpcResult<Value> {
    get_current_user_id(ctx)?;
    service.component_view(false).await.map_err(RpcError::from)
}
