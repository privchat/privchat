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

pub mod by_qrcode;
pub mod query;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 search 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let services_qrcode = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_search::BY_QRCODE, move |body, ctx| {
            let services = services_qrcode.clone();
            Box::pin(async move { by_qrcode::handle(body, services, ctx).await })
        })
        .await;

    let services_query = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_search::QUERY, move |body, ctx| {
            let services = services_query.clone();
            Box::pin(async move { query::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("🔧 Search 模块路由注册完成");
}
