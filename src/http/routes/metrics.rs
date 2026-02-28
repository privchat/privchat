// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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
