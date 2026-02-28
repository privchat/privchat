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

//! 频道系统 RPC 接口（私聊、群聊等会话功能）

pub mod direct;
pub mod hide;
pub mod mute;
pub mod pin;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册频道系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // ✨ 获取或创建私聊会话
    let services_direct = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::channel::DIRECT_GET_OR_CREATE, move |params, ctx| {
            let services = services_direct.clone();
            Box::pin(async move { direct::handle(params, services, ctx).await })
        })
        .await;

    // channel/list 已废弃，列表数据由 entity/sync_entities 同步，客户端从本地读 get_channels

    // ✨ 置顶频道 - 使用路由常量
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::channel::PIN, move |params, ctx| {
            let services = services_clone.clone();
            Box::pin(async move { pin::handle(params, services, ctx).await })
        })
        .await;

    // ✨ 隐藏频道 - 使用路由常量
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::channel::HIDE, move |params, ctx| {
            let services = services_clone.clone();
            Box::pin(async move { hide::handle(params, services, ctx).await })
        })
        .await;

    // ✨ 设置频道静音 - 使用路由常量
    let services_clone = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::channel::MUTE, move |params, ctx| {
            let services = services_clone.clone();
            Box::pin(async move { mute::handle(params, services, ctx).await })
        })
        .await;

    tracing::debug!("💬 Channel 系统路由注册完成 (direct/get_or_create, pin, hide, mute)");
}
