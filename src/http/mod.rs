//! HTTP 服务器模块
//!
//! 包含两个独立的 HTTP 服务：
//! - 文件服务（对外）：文件上传、下载、URL 获取、删除
//! - 管理 API（仅内网）：用户管理、群组管理、统计等

pub mod dto;
pub mod middleware;
pub mod routes;
pub mod server;

pub use server::{AdminHttpServer, AdminServerState, FileHttpServer, FileServerState};
