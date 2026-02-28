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

//! 表情包系统 RPC 接口

pub mod package;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册表情包系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // 表情包库列表
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register("sticker/package/list", move |params, _ctx| {
            let services = services_clone.clone();
            Box::pin(async move { package::list::handle(services, params).await })
        })
        .await;

    // 表情包库详情
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register("sticker/package/detail", move |params, _ctx| {
            let services = services_clone.clone();
            Box::pin(async move { package::detail::handle(services, params).await })
        })
        .await;

    tracing::debug!("📦 Sticker 系统路由注册完成");
}
