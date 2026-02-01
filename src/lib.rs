#![allow(unused_variables, dead_code, async_fn_in_trait, static_mut_refs)]

pub mod auth;
pub mod cli;
pub mod config;
pub mod context;
pub mod dispatcher;
pub mod error;
pub mod handler;
pub mod http;  // HTTP 文件服务器
pub mod infra;
pub mod logging;
pub mod middleware;
pub mod model;
pub mod offline;
pub mod repository;
pub mod security;  // 安全模块
pub mod server;
pub mod service;
pub mod session;
pub mod sync;
pub mod rpc;  // 添加 RPC 模块
pub mod domain;  // ✨ 新增：Domain Events
pub mod push;  // ✨ 新增：Push 模块

pub use config::ServerConfig;
pub use context::RequestContext;
pub use dispatcher::{
    MessageDispatcher, MessageDispatcherBuilder,
    middleware::{
        LoggingMiddleware, AuthenticationMiddleware, 
        RateLimitMiddleware, ConnectionMiddleware
    }
};
pub use error::{Result, ServerError};
pub use handler::{
    MessageHandler,
    ConnectMessageHandler, DisconnectMessageHandler, 
    PingMessageHandler, SendMessageHandler, SubscribeMessageHandler
};
pub use infra::{
    CacheManager, OnlineStatusManager
};
pub use model::*;
pub use server::ChatServer;
pub use session::SessionManager; 