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

#![allow(unused_variables, dead_code, async_fn_in_trait, static_mut_refs)]

pub mod auth;
pub mod cli;
pub mod config;
pub mod context;
pub mod dispatcher;
pub mod domain; // ✨ 新增：Domain Events
pub mod error;
pub mod handler;
pub mod http; // HTTP 文件服务器
pub mod infra;
pub mod logging;
pub mod middleware;
pub mod model;
pub mod offline;
pub mod push;
pub mod repository;
pub mod rpc; // 添加 RPC 模块
pub mod security; // 安全模块
pub mod server;
pub mod service;
pub mod session;
pub mod sync; // ✨ 新增：Push 模块

pub use config::ServerConfig;
pub use context::RequestContext;
pub use dispatcher::{
    middleware::{
        AuthenticationMiddleware, ConnectionMiddleware, LoggingMiddleware, RateLimitMiddleware,
    },
    MessageDispatcher, MessageDispatcherBuilder,
};
pub use error::{Result, ServerError};
pub use handler::{
    ConnectMessageHandler, DisconnectMessageHandler, MessageHandler, PingMessageHandler,
    SendMessageHandler, SubscribeMessageHandler,
};
pub use infra::{CacheManager, OnlineStatusManager};
pub use model::*;
pub use server::ChatServer;
pub use session::SessionManager;
