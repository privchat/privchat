//! Service-key-only component management. Reuses the existing service envelope.
use super::admin::verify_service_key_with_manager;
use crate::auth::ServiceKeyManager;
use crate::http::{AdminServerState, ApiEnvelope, ApiResult};
use crate::service::call_service::{components::UpdateRtcSettings, CallService};
use axum::{
    extract::{FromRef, State},
    http::HeaderMap,
    Json,
};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone)]
pub struct ComponentConfigState {
    pub service_key_manager: Arc<ServiceKeyManager>,
    pub service: Arc<CallService>,
}
impl FromRef<AdminServerState> for ComponentConfigState {
    fn from_ref(state: &AdminServerState) -> Self {
        Self {
            service_key_manager: state.service_key_manager.clone(),
            service: state.component_config.clone(),
        }
    }
}

pub async fn get_components(
    State(state): State<ComponentConfigState>,
    headers: HeaderMap,
) -> ApiResult<Value> {
    verify_service_key_with_manager(&headers, &state.service_key_manager).await?;
    Ok(ApiEnvelope::ok(state.service.component_view(true).await?))
}

pub async fn update_rtc_component(
    State(state): State<ComponentConfigState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> ApiResult<Value> {
    // Authenticate before even decoding the privileged payload.
    verify_service_key_with_manager(&headers, &state.service_key_manager).await?;
    let Json(request) = Json::<UpdateRtcSettings>::from_bytes(&body).map_err(|_| {
        crate::error::ServerError::Validation("Invalid RTC settings request".into())
    })?;
    Ok(ApiEnvelope::ok(state.service.update_rtc(request).await?))
}
