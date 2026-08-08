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

    // 单条转发路由（MEDIA_REFERENCE_AND_FORWARD_SPEC §6）：**无条件不注册**。
    //
    // 🔴 这里曾经是个配置开关（`[message] forward_enabled`）。开关本身没错，
    // 错在它当时是「未完成的安全边界」的唯一闸门——一行配置就能启用一个
    // 明确缺少发送权限校验（禁言/黑名单/角色）、缺少内容保护、且非原子的 RPC。
    // 运行期开关的正当用途是**安全功能做完之后**的灰度放量，不是替代没做完的活。
    //
    // 恢复注册的前提（spec §15 发布门禁）：
    //   1. 转发与普通发送共用同一个 `authorize_send_to_channel()`
    //   2. §6.3 内容保护（FORWARDS_RESTRICTED）落地
    //   3. 单事务 `forward_message_atomic`（锁源 → 校验 → 幂等 → 写入）
    // 三项齐了再把开关加回来，作为灰度手段。

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
        "📋 Message 系统路由注册完成 (history, status, reaction, revoke, pin, pin/list；forward 未注册)"
    );
}
