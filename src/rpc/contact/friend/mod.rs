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

pub mod accept;
pub mod apply;
pub mod check;
pub mod pending;
pub mod remove;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册好友模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    // ✨ 使用路由常量代替硬编码字符串
    router
        .register(routes::friend::APPLY, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { apply::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::friend::ACCEPT, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { accept::handle(body, services, ctx).await })
            })
        })
        .await;

    // contact/friend/list 已废弃，列表数据由 entity/sync_entities 同步，客户端从本地读 get_friends
    // router.register(routes::friend::LIST, ...) 已移除

    router
        .register(routes::friend::DELETE, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { remove::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::friend::PENDING, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { pending::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::friend::CHECK, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { check::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Friend 模块路由注册完成 (使用 privchat-protocol routes)");
}
