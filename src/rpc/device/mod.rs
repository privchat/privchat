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

// 设备管理 RPC 模块

pub mod kick_device;
pub mod kick_other_devices;
pub mod list;
pub mod push; // ✨ Phase 3.5: 推送状态管理

pub use kick_device::handle as handle_kick_device;
pub use kick_other_devices::handle as handle_kick_other_devices;
pub use list::handle as handle_list_devices;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册设备管理的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    // device/list - 获取设备列表
    GLOBAL_RPC_ROUTER
        .register("device/list", {
            let services = services.clone();
            move |body: serde_json::Value, _ctx: crate::rpc::RpcContext| {
                let services = services.clone();
                async move { list::handle(&services, body).await }
            }
        })
        .await;

    // device/kick_other_devices - 踢出其他设备 ✨ 新增
    GLOBAL_RPC_ROUTER
        .register("device/kick_other_devices", {
            let services = services.clone();
            move |body: serde_json::Value, ctx: crate::rpc::RpcContext| {
                let services = services.clone();
                async move { kick_other_devices::handle(body, services, ctx).await }
            }
        })
        .await;

    // device/kick_device - 踢出指定设备 ✨ 新增
    GLOBAL_RPC_ROUTER
        .register("device/kick_device", {
            let services = services.clone();
            move |body: serde_json::Value, ctx: crate::rpc::RpcContext| {
                let services = services.clone();
                async move { kick_device::handle(body, services, ctx).await }
            }
        })
        .await;

    // ✨ Phase 3.5: device/push/update - 更新设备推送状态
    GLOBAL_RPC_ROUTER
        .register(routes::device::PUSH_UPDATE, {
            let services = services.clone();
            move |body: serde_json::Value, ctx: crate::rpc::RpcContext| {
                let services = services.clone();
                async move { push::handle_push_update(body, services, ctx).await }
            }
        })
        .await;

    // ✨ Phase 3.5: device/push/status - 获取设备推送状态
    GLOBAL_RPC_ROUTER
        .register(routes::device::PUSH_STATUS, {
            let services = services.clone();
            move |body: serde_json::Value, ctx: crate::rpc::RpcContext| {
                let services = services.clone();
                async move { push::handle_push_status(body, services, ctx).await }
            }
        })
        .await;

    tracing::debug!("📋 Device 系统路由注册完成 (list, kick_other_devices, kick_device, push/update, push/status)");
}
