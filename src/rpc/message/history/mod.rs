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

pub mod around;
pub mod get;
pub mod search;

use super::super::router::GLOBAL_RPC_ROUTER;
use super::super::RpcServiceContext;

/// 历史消息的统一 JSON 视图（get / around 共用，保证 SDK 回填时字段一致）。
/// 撤回消息清空 content（客户端按 revoked 显示本地化占位符）；
/// `message_seq` = per-channel pts，命名与 SendMessageResponse.message_seq 对齐。
pub(crate) fn message_view_json(msg: &crate::model::Message) -> serde_json::Value {
    let content = if msg.revoked {
        String::new()
    } else {
        msg.content.clone()
    };
    serde_json::json!({
        "message_id": msg.message_id,
        "channel_id": msg.channel_id,
        "sender_id": msg.sender_id,
        "content": content,
        "message_type": msg.message_type.as_str(),
        "timestamp": msg.created_at.timestamp_millis(),
        "message_seq": msg.pts,
        "reply_to_message_id": msg.reply_to_message_id,
        "metadata": msg.metadata,
        "revoked": msg.revoked,
        "revoked_at": msg.revoked_at.map(|dt| dt.timestamp_millis()),
        "revoked_by": msg.revoked_by
    })
}

/// 注册 history 模块的所有路由
pub async fn register_routes(services: RpcServiceContext) {
    let router = GLOBAL_RPC_ROUTER.clone();

    router
        .register("message/history/get", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { get::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("message/history/search", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { search::handle(body, services, ctx).await })
            })
        })
        .await;

    router
        .register("message/history/around", {
            let services = services.clone();
            Box::new(move |body, ctx| {
                let services = services.clone();
                Box::pin(async move { around::handle(body, services, ctx).await })
            })
        })
        .await;

    tracing::debug!(
        "📋 history 模块路由注册完成（get / search / around，MESSAGE_HISTORY spec §3-§5）"
    );
}
