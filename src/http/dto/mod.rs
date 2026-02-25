//! DTO (Data Transfer Objects) 模块
//!
//! 定义所有 HTTP API 的请求/响应结构体，
//! 使用 `#[derive(Serialize, Deserialize)]` 保证类型安全。

pub mod admin;
