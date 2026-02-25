//! HTTP 路由模块
//!
//! 路由结构：
//! - 文件服务（端口 9083，对外）：
//!   - `/api/app/files/upload` - 文件上传
//!   - `/api/app/files/{file_id}` - 文件下载/删除
//!   - `/api/app/files/{file_id}/url` - 获取文件 URL
//!   - `/metrics` - Prometheus 指标
//! - 管理 API（端口 9090，仅内网）：
//!   - `/api/admin/*` - 管理接口（业务系统调用，使用 X-Service-Key 认证）

pub mod admin;
pub mod delete;
pub mod download;
pub mod metrics;
pub mod upload;
pub mod url;

use crate::http::{AdminServerState, FileServerState};
use axum::{routing::get, Router};

/// 创建文件服务路由
pub fn create_file_routes() -> Router<FileServerState> {
    Router::new()
        .route("/metrics", get(metrics::metrics_handler))
        .merge(upload::create_route())
        .merge(download::create_route())
        .merge(url::create_route())
        .merge(delete::create_route())
}

/// 创建管理 API 路由
pub fn create_admin_routes() -> Router<AdminServerState> {
    admin::create_route()
}
