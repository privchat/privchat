//! HTTP 服务器模块 - 使用 Axum 提供文件服务 API
//!
//! 功能包括：
//! - 文件上传接口
//! - 文件下载接口
//! - 文件 URL 获取接口
//! - 文件删除接口

pub mod routes;
pub mod middleware;
pub mod server;

pub use server::{FileHttpServer, HttpServerState};

