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

#![allow(unused_variables, dead_code, async_fn_in_trait)]

pub mod auth;
pub mod channel_transfer; // Channel Transfer relay utilities (spec 02-server/CHANNEL_TRANSFER_SPEC v2.0).
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
pub mod server_event; // server → downstream 通用事件分发 (spec SERVER_EVENT_DISPATCH_SPEC)
pub mod service;
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
pub use infra::CacheManager;
pub use model::*;
pub use server::ChatServer;

/// 真库测试的数据库地址。**拿不到就 panic**，不是静默跳过。
///
/// 🔴 为什么不能跳过：跳过的测试在报表里记的是「通过」。于是
/// 「396 passed / 0 failed」既可能表示 SQL 契约全部执行过，也可能表示
/// 一条都没跑——两者在输出上不可区分，而发布门禁恰恰要靠这个数说话。
///
/// 本地确实没有 Postgres 时，显式设 `PRIVCHAT_ALLOW_SKIPPING_DB_TESTS=1`：
/// 那是一次**自觉的降级**，而不是默认行为。
pub fn require_test_database_url() -> Option<String> {
    if let Ok(url) = std::env::var("PRIVCHAT_TEST_DATABASE_URL") {
        return Some(url);
    }
    if let Ok(url) = std::env::var("DATABASE_URL") {
        return Some(url);
    }
    if std::env::var("PRIVCHAT_ALLOW_SKIPPING_DB_TESTS").is_ok() {
        eprintln!(
            "⚠️ 跳过真库测试：PRIVCHAT_ALLOW_SKIPPING_DB_TESTS 已设置。\
             这一轮的结果**不能**用作发布门禁证据。"
        );
        return None;
    }
    panic!(
        "真库测试需要 DATABASE_URL / PRIVCHAT_TEST_DATABASE_URL。\n\
         缺少数据库时默认失败而不是跳过——跳过会被记成通过，\n\
         让「全绿」既可能是「SQL 契约都验过」也可能是「一条都没跑」。\n\
         本地确实没有 Postgres 时显式设 PRIVCHAT_ALLOW_SKIPPING_DB_TESTS=1。"
    );
}

#[cfg(test)]
pub(crate) fn database_fixture_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;

    // A small set of DB-backed state-machine tests deliberately uses fixed
    // primary keys so they can assert fencing and cascade behavior. They must
    // not run concurrently against the same DATABASE_URL.
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
