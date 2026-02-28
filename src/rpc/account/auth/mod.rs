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

pub mod login;
pub mod logout;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 auth 模块的所有路由
/// 注意：这些接口主要用于本地测试，生产环境应该使用 AuthorizationRequest 中的认证机制
pub async fn register_routes(services: RpcServiceContext) {
    let services_login = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::auth::LOGIN, move |body, ctx| {
            let services = services_login.clone();
            Box::pin(async move { login::handle(body, services, ctx).await })
        })
        .await;

    let services_logout = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::auth::LOGOUT, move |body, ctx| {
            let services = services_logout.clone();
            Box::pin(async move { logout::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("🔧 Auth 模块路由注册完成（测试用）");
}
