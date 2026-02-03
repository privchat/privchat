//! HTTP 路由模块
//! 
//! 路由结构：
//! - `/api/admin/*` - 管理接口（业务系统调用，使用 X-Service-Key 认证）
//!   - `/api/admin/token/issue` - Token 签发接口
//!   - `/api/admin/users/*` - 用户管理
//!   - `/api/admin/groups/*` - 群组管理
//!   - `/api/admin/friendships/*` - 好友管理
//!   - `/api/admin/login-logs/*` - 登录日志
//!   - `/api/admin/devices/*` - 设备管理
//!   - `/api/admin/stats/*` - 统计报表
//!   - `/api/admin/messages/*` - 聊天记录
//! - `/api/app/*`   - 客户端接口（客户端调用，需要上传 token 等认证）

pub mod admin;
pub mod upload;
pub mod download;
pub mod url;
pub mod delete;
pub mod metrics;

use axum::{Router, routing::get};
use crate::http::HttpServerState;

/// 创建所有路由
pub fn create_routes() -> Router<HttpServerState> {
    Router::new()
        .route("/metrics", get(metrics::metrics_handler))
        .merge(admin::create_route())      // /api/admin/* - 管理 API
        .merge(upload::create_route())     // /api/app/files/upload - 文件上传
        .merge(download::create_route())   // /api/app/files/{file_id} - 文件下载
        .merge(url::create_route())        // /api/app/files/{file_id}/url - 获取文件 URL
        .merge(delete::create_route())     // /api/app/files/{file_id} - 删除文件
}

