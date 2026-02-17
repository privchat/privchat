//! GET /metrics - Prometheus 抓取端点

use axum::{
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};

/// GET /metrics：返回 Prometheus 文本格式指标。
/// 若未初始化指标（init 未调用），返回 503。
pub async fn metrics_handler() -> Response {
    if let Some(body) = crate::infra::metrics::render_metrics() {
        (
            StatusCode::OK,
            [(
                header::CONTENT_TYPE,
                "text/plain; version=0.0.4; charset=utf-8",
            )],
            body,
        )
            .into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "metrics not initialized").into_response()
    }
}
