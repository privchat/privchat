// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! 群二维码 RPC 模块（QR_CODE_SPEC v1.3）。

pub mod get;
pub mod join;
pub mod refresh;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 注册群二维码模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("group/qrcode/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { get::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/qrcode/refresh", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { refresh::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("group/join/qrcode", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { join::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Group QRCode 路由注册完成 (get / refresh / group/join/qrcode)");
}
