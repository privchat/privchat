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

pub mod generate;
pub mod list;
pub mod refresh;
pub mod resolve;
pub mod revoke;
pub mod utils;

// 重新导出工具函数
pub use utils::{extract_qr_key_from_url, extract_token_from_url, generate_random_token};

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册 QR 码模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    // 生成 QR 码
    router
        .register("qrcode/generate", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { generate::handle(body, services, ctx).await })
            })
        })
        .await;

    // 解析 QR 码
    router
        .register("qrcode/resolve", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { resolve::handle(body, services, ctx).await })
            })
        })
        .await;

    // 刷新 QR 码
    router
        .register("qrcode/refresh", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { refresh::handle(body, services, ctx).await })
            })
        })
        .await;

    // 撤销 QR 码
    router
        .register("qrcode/revoke", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { revoke::handle(body, services, ctx).await })
            })
        })
        .await;

    // 列出 QR 码
    router
        .register("qrcode/list", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { list::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 QRCode 模块路由注册完成 (generate, resolve, refresh, revoke, list)");
}
