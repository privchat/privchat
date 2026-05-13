// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0.

//! `account/bot/*` RPC routes (spec `02-server/SERVICE_ACCOUNT_FOLLOW_SPEC`).

pub mod follow;
pub mod unfollow;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册 Bot 关注 / 取消关注路由。
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register(routes::account_bot::FOLLOW, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { follow::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register(routes::account_bot::UNFOLLOW, {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { unfollow::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!("📋 Bot follow 路由注册完成 (account/bot/follow + unfollow)");
}
