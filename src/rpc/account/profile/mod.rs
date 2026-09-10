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

pub mod update;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册个人资料模块的路由。
///
/// 🔴 只注册 UPDATE。
///
/// 同目录下的 `get` 还是个返回固定 JSON 的桩：注册它等于给客户端一个永远"成功"
/// 但没有数据的资料接口——这正是整个模块之前被整体注释掉的原因。读资料走
/// `account/user/detail`（有真实实现与可见性投影）。
pub async fn register_routes(services: RpcServiceContext) {
    let services_update = services.clone();
    GLOBAL_RPC_ROUTER
        .register(routes::account_profile::UPDATE, move |body, ctx| {
            let services = services_update.clone();
            Box::pin(async move { update::handle(body, services, ctx).await })
        })
        .await;

    tracing::debug!("📋 Profile 模块路由注册完成 (update)");
}
