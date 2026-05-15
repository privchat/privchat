// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! 个人名片二维码 RPC 模块（QR_CODE_SPEC v1.3）。

pub mod qrcode;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;

/// 注册用户模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("user/qrcode/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::get(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("user/qrcode/refresh", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::refresh(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("user/qrcode/resolve", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { qrcode::resolve(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 User QRCode 路由注册完成 (get / refresh / resolve)");
}
