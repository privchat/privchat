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

pub mod qrcode;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册用户模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    // 生成用户二维码
    router
        .register("user/qrcode/generate", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::generate(body, services, ctx).await })
            })
        })
        .await;

    // 刷新用户二维码
    router
        .register("user/qrcode/refresh", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::refresh(body, services, ctx).await })
            })
        })
        .await;

    // 获取用户二维码
    router
        .register("user/qrcode/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::get(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 User 模块路由注册完成 (qrcode/generate, qrcode/refresh, qrcode/get)");
}
