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

pub mod history;
pub mod pin;
pub mod reaction;
pub mod revoke;
pub mod status;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册消息系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    history::register_routes(services.clone()).await;
    status::register_routes(services.clone()).await;
    reaction::register_routes(services.clone()).await;

    // 注册消息撤回路由
    GLOBAL_RPC_ROUTER
        .register(routes::message::REVOKE, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { revoke::handle(params, services, ctx).await })
            }
        })
        .await;

    // 注册群消息置顶 / 取消置顶路由（P1）
    GLOBAL_RPC_ROUTER
        .register(routes::message::PIN, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { pin::handle(params, services, ctx).await })
            }
        })
        .await;

    // 注册群置顶消息列表路由（P1）
    GLOBAL_RPC_ROUTER
        .register(routes::message::PIN_LIST, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { pin::handle_list(params, services, ctx).await })
            }
        })
        .await;

    tracing::debug!(
        "📋 Message 系统路由注册完成 (history, status, reaction, revoke, pin, pin/list)"
    );
}
