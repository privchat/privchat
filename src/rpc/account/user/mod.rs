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

pub mod detail;
pub mod register;
pub mod share_card;
pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册用户模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 为每个处理器创建闭包，捕获服务上下文
    let services_register = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_user::REGISTER, move |body, ctx| {
            let services = services_register.clone();
            async move { register::handle(body, services, ctx).await }
        })
        .await;

    let services_detail = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_user::DETAIL, move |body, ctx| {
            let services = services_detail.clone();
            Box::pin(async move { detail::handle(body, services, ctx).await })
        })
        .await;

    let services_update = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_user::UPDATE, move |body, ctx| {
            let services = services_update.clone();
            async move { update::handle(body, services, ctx).await }
        })
        .await;

    let services_share_card = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_user::SHARE_CARD, move |body, ctx| {
            let services = services_share_card.clone();
            Box::pin(async move { share_card::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("🔧 User 模块路由注册完成");
}
