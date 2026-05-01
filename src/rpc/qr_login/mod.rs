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

//! 扫码登录 RPC 模块（spec QR_API §5）。
//!
//! 当前唯一接口：[`create_scene`]，由 Web/PC 在未认证 RPC 连接上调用。

pub mod create_scene;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 qr_login 模块的所有路由（仅 create_scene；scan/confirm/reject 通过
/// admin HTTP 走，由 application 透传，不在 unauth RPC 暴露）。
pub async fn register_routes(services: RpcServiceContext) {
    let services_create = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::qr_login::CREATE_SCENE, move |body, ctx| {
            let services = services_create.clone();
            Box::pin(async move { create_scene::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("🔧 qr_login 模块路由注册完成（create_scene）");
}
