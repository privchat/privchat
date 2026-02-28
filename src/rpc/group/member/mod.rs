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

pub mod add;
pub mod leave;
pub mod list;
pub mod mute;
pub mod remove;
pub mod unmute;

use super::super::RpcServiceContext;

/// 注册 member 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = crate::rpc::GLOBAL_RPC_ROUTER.clone();

    router
        .register("group/member/add", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { add::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/remove", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { remove::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/list", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/leave", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { leave::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/mute", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { mute::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/member/unmute", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { unmute::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 member 模块路由注册完成 (add, remove, list, leave, mute, unmute)");
}
