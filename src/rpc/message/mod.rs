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

    // 单条转发**不再是独立路由**（MEDIA_REFERENCE_AND_FORWARD_SPEC §6）。
    //
    // 转发就是「按源消息的内容再发一条」，走的应该是现成的发送链路：
    // `sync/submit` 已经是 RPC，已经带幂等命名空间、发送权限、投递、回执、
    // difference 与多设备同步。再开一条 `message/forward` 等于把这些全部重写一遍——
    // 之前列出的那一长串启用前提（真单事务、带指纹的幂等、canonical 投影、
    // 单 SQL 成员判定、typed 错误码），逐条都是「把发送链路已有的东西再造一次」。
    //
    // 收口形态：`sync/submit` 的一个 command，payload 只带
    // `source_channel_id + source_message_id`；服务端校验源可读、目标可发，
    // 复制正文与媒体引用，重新生成 id/pts/时间/发送者。
    // 🔴 客户端永远不自己报 `file_id`——那等于可以引用别人的附件。
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
        "📋 Message 系统路由注册完成 (history, status, reaction, revoke, pin, pin/list)"
    );
}
