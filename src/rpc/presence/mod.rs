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

pub mod status;
pub mod typing;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册在线状态系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // presence/typing - 输入状态通知
    GLOBAL_RPC_ROUTER
        .register(routes::presence::TYPING, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { typing::handle(params, services, ctx).await })
            }
        })
        .await;

    // presence/status/get - 批量查询在线状态
    GLOBAL_RPC_ROUTER
        .register(routes::presence::STATUS_GET, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { status::handle(params, services, ctx).await })
            }
        })
        .await;

    tracing::debug!("🔔 Presence 系统路由注册完成 (typing, status)");
}
