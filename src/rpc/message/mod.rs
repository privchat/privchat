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

pub mod forward;
pub mod history;
pub mod pin;
pub mod reaction;
pub mod revoke;
pub mod status;

use super::router::GLOBAL_RPC_ROUTER;
use super::RpcServiceContext;
use privchat_protocol::rpc::routes;

/// 注册消息系统的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    history::register_routes(services.clone()).await;
    status::register_routes(services.clone()).await;
    reaction::register_routes(services.clone()).await;

    // 注册消息撤回路由
    GLOBAL_RPC_ROUTER
        .register(routes::message::REVOKE, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { revoke::handle(params, services, ctx).await })
            }
        })
        .await;

    // 单条转发路由（MEDIA_REFERENCE_AND_FORWARD_SPEC §6 / §15.1）。
    //
    // 启用前提（都已满足，改动其一必须重新评估）：
    //   1. 与普通发送**共用** `service::send_authorization::authorize_send_to_channel`
    //      —— 禁言、全员禁言、角色权限、频道设置、私聊的好友/拉黑/隐私一条不少
    //   2. §6.3 内容保护：源群 `forbid_forward` 时返回 FORWARDS_RESTRICTED
    //   3. 提交事务内锁源消息，复查存活 + 正文 + metadata + 引用集合
    //      （`ForwardPrecondition`），撤回/编辑与转发被行锁排成序
    //
    // 🔴 这里一度是个配置开关。开关本身没错，错在它当时是「未完成的安全边界」
    // 的唯一闸门——一行配置就能启用缺少上述校验的写入口。开关的正当用途是
    // 功能做完之后的灰度放量。
    GLOBAL_RPC_ROUTER
        .register(routes::message::FORWARD, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { forward::handle(params, services, ctx).await })
            }
        })
        .await;

    // 注册群消息置顶 / 取消置顶路由（P1）
    GLOBAL_RPC_ROUTER
        .register(routes::message::PIN, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { pin::handle(params, services, ctx).await })
            }
        })
        .await;

    // 注册群置顶消息列表路由（P1）
    GLOBAL_RPC_ROUTER
        .register(routes::message::PIN_LIST, {
            let services = services.clone();
            move |params, ctx| {
                let services = services.clone();
                Box::pin(async move { pin::handle_list(params, services, ctx).await })
            }
        })
        .await;

    tracing::debug!(
        "📋 Message 系统路由注册完成 (history, status, reaction, revoke, forward, pin, pin/list)"
    );
}
