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

//! Token 能力矩阵（generic：不含任何业务域语义）
//!
//! 统一 token 早已带 `scope`（`IssueParams.scope`，默认 `["user"]`），但认证层此前只校验
//! “是否已认证”，不按 scope 授权 —— 任何持票人都能调用全部路由。本模块补上 scope → 能力
//! 的授权矩阵，供 `AuthMiddleware` 在认证之后、分发之前执行。
//!
//! 设计约束：
//!
//! 1. **能力属于 token，不属于身份。** 受限用户的 `user_type` 仍是 `NORMAL`；
//!    `user_type` 只做身份分类（0=NORMAL / 1=SYSTEM / 2=BOT）。
//! 2. **完整能力只认显式 scope。** `user` / `im` 与历史上不带 scope 的 token 是完整 IM 能力；
//!    **未知 scope 没有任何能力**——否则只要签一个拼错或新造的 scope 就等于全权限，
//!    "受限凭据"无从谈起（WEEY_AGENT_SPEC §9、AUTH_SPEC 受限能力）。
//! 3. **白名单而非黑名单。** 受限 scope 新增路由默认拒绝，避免新路由悄悄放行。

use privchat_protocol::protocol::MessageType;
use privchat_protocol::rpc::routes;
use std::collections::HashSet;

/// 完整 IM 能力的 scope（存量 token 用的就是这些）
pub const SCOPE_USER: &str = "user";
pub const SCOPE_IM: &str = "im";

/// 受限 scope：只能在已是成员的会话里收发消息
///
/// 典型用途是由业务侧代开的、不可自助扩散的账号（如客服访客）。本模块不关心它是谁，
/// 只关心它的能力边界。
pub const SCOPE_MESSAGING: &str = "messaging";

/// 受限 scope：设备执行节点（私人助理 assistant_uid + deviceId 的节点凭据）
///
/// 只能建立连接、订阅获准的频道、收发 Transfer。不能读写聊天、查资料、建关系，
/// 也不能经 server 内置 refresh 续期——续期由业务侧按节点授权重新校验后签发。
pub const SCOPE_AGENT_NODE: &str = "agent_node";

/// scope 是否代表完整 IM 能力
fn is_full_access_scope(scope: &str) -> bool {
    matches!(scope, SCOPE_USER | SCOPE_IM)
}

lazy_static::lazy_static! {
    /// `messaging` scope 允许的 RPC 路由
    ///
    /// 覆盖：已在会话内的收发、历史同步、自有会话的媒体、自身资料与在线状态。
    /// 不覆盖（即拒绝）：用户搜索、好友关系、群创建与管理、自助建单聊、
    /// 二维码身份扩散、频道广播与内容发布。
    static ref MESSAGING_ROUTES: HashSet<&'static str> = {
        let mut set = HashSet::new();

        // 历史与已读同步
        set.insert(routes::message_history::GET);
        set.insert(routes::message_history::AROUND);
        set.insert(routes::message_status::READ_PTS);
        set.insert(routes::message_status::COUNT);
        set.insert(routes::message_status::READ_LIST);
        set.insert(routes::message_status::READ_STATS);

        // 会话内消息操作
        set.insert(routes::message::REVOKE);
        // 转发不再有独立路由：它是 `sync/submit` 的一个 command，
        // 能力由 `routes::sync::SUBMIT` 覆盖（见下方「增量同步」）。
        set.insert(routes::message_reaction::ADD);
        set.insert(routes::message_reaction::REMOVE);
        set.insert(routes::message_reaction::LIST);
        set.insert(routes::message_reaction::STATS);

        // 增量同步
        set.insert(routes::sync::SUBMIT);
        set.insert(routes::sync::GET_DIFFERENCE);
        set.insert(routes::sync::GET_CHANNEL_PTS);
        set.insert(routes::sync::BATCH_GET_CHANNEL_PTS);
        set.insert(routes::sync::SESSION_READY);
        set.insert(routes::entity::SYNC_ENTITIES);

        // 自有会话的媒体（服务端仍按会话归属二次校验）
        set.insert(routes::file::REQUEST_UPLOAD_TOKEN);
        set.insert(routes::file::UPLOAD_CALLBACK);
        set.insert(routes::file::GET_URL);

        // 自身会话视图与在线状态
        set.insert(routes::channel::PIN);
        set.insert(routes::channel::HIDE);
        set.insert(routes::channel::MUTE);
        set.insert(routes::presence::TYPING);
        set.insert(routes::presence::STATUS_GET);

        // 自身资料（对端资料读取仍由既有的会话成员校验闸口把关）
        set.insert(routes::account_profile::GET);
        set.insert(routes::account_profile::UPDATE);
        set.insert(routes::account_user::DETAIL);

        // 会话生命周期与推送
        set.insert(routes::auth::LOGOUT);
        set.insert(routes::auth::REFRESH);
        set.insert(routes::device::PUSH_UPDATE);
        set.insert(routes::device::PUSH_STATUS);

        set
    };

    /// `agent_node` scope 允许的 RPC 路由：只有登出。
    static ref AGENT_NODE_ROUTES: HashSet<&'static str> = {
        let mut set = HashSet::new();
        set.insert(routes::auth::LOGOUT);
        set
    };

    /// `agent_node` scope 允许的入站消息类型：连接维持、频道订阅与 Transfer。
    /// 不含 SendMessage / Publish——节点不以 IM 身份发言或广播。
    static ref AGENT_NODE_MESSAGE_TYPES: HashSet<MessageType> = {
        let mut set = HashSet::new();
        set.insert(MessageType::AuthorizationRequest);
        set.insert(MessageType::DisconnectRequest);
        set.insert(MessageType::PingRequest);
        set.insert(MessageType::RpcRequest);
        set.insert(MessageType::SubscribeRequest);
        set.insert(MessageType::TransferRequest);
        set
    };

    /// `messaging` scope 允许的入站消息类型
    static ref MESSAGING_MESSAGE_TYPES: HashSet<MessageType> = {
        let mut set = HashSet::new();
        set.insert(MessageType::AuthorizationRequest);
        set.insert(MessageType::DisconnectRequest);
        set.insert(MessageType::SendMessageRequest);
        set.insert(MessageType::PingRequest);
        set.insert(MessageType::RpcRequest);
        set
    };
}

/// token 的 scope 列表是否允许访问该 RPC 路由
///
/// 多 scope 取并集：任一 scope 放行即放行。
pub fn allows_rpc_route(scopes: &[String], route: &str) -> bool {
    if scopes.is_empty() {
        // 无 scope 的历史 token：按完整能力处理
        return true;
    }

    scopes.iter().any(|scope| {
        if is_full_access_scope(scope) {
            return true;
        }
        match scope.as_str() {
            SCOPE_MESSAGING => MESSAGING_ROUTES.contains(route),
            SCOPE_AGENT_NODE => AGENT_NODE_ROUTES.contains(route),
            _ => false,
        }
    })
}

/// token 的 scope 列表是否允许发送该消息类型
pub fn allows_message_type(scopes: &[String], msg_type: &MessageType) -> bool {
    if scopes.is_empty() {
        return true;
    }

    scopes.iter().any(|scope| {
        if is_full_access_scope(scope) {
            return true;
        }
        match scope.as_str() {
            SCOPE_MESSAGING => MESSAGING_MESSAGE_TYPES.contains(msg_type),
            SCOPE_AGENT_NODE => AGENT_NODE_MESSAGE_TYPES.contains(msg_type),
            _ => false,
        }
    })
}

/// 列出某个受限 scope 的全部允许路由（诊断与测试用）
pub fn list_scope_routes(scope: &str) -> Vec<&'static str> {
    match scope {
        SCOPE_MESSAGING => {
            let mut v: Vec<&'static str> = MESSAGING_ROUTES.iter().copied().collect();
            v.sort_unstable();
            v
        }
        SCOPE_AGENT_NODE => {
            let mut v: Vec<&'static str> = AGENT_NODE_ROUTES.iter().copied().collect();
            v.sort_unstable();
            v
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messaging() -> Vec<String> {
        vec![SCOPE_MESSAGING.to_string()]
    }

    #[test]
    fn full_access_scopes_allow_everything() {
        for scope in [SCOPE_USER, SCOPE_IM] {
            let scopes = vec![scope.to_string()];
            assert!(allows_rpc_route(&scopes, routes::account_search::QUERY));
            assert!(allows_rpc_route(&scopes, routes::group::CREATE));
            assert!(allows_rpc_route(
                &scopes,
                routes::channel::DIRECT_GET_OR_CREATE
            ));
            assert!(allows_message_type(&scopes, &MessageType::SubscribeRequest));
        }
    }

    #[test]
    fn empty_scope_is_backward_compatible() {
        let scopes: Vec<String> = vec![];
        assert!(allows_rpc_route(&scopes, routes::group::CREATE));
        assert!(allows_message_type(&scopes, &MessageType::PublishRequest));
    }

    #[test]
    fn messaging_scope_allows_conversation_traffic() {
        let scopes = messaging();
        assert!(allows_rpc_route(&scopes, routes::message_history::GET));
        assert!(allows_rpc_route(&scopes, routes::sync::GET_DIFFERENCE));
        assert!(allows_rpc_route(&scopes, routes::file::REQUEST_UPLOAD_TOKEN));
        assert!(allows_rpc_route(&scopes, routes::account_profile::UPDATE));
        assert!(allows_message_type(
            &scopes,
            &MessageType::SendMessageRequest
        ));
    }

    #[test]
    fn messaging_scope_denies_discovery_and_group_creation() {
        let scopes = messaging();
        // 用户搜索
        assert!(!allows_rpc_route(&scopes, routes::account_search::QUERY));
        assert!(!allows_rpc_route(&scopes, routes::account_search::BY_QRCODE));
        // 好友关系
        assert!(!allows_rpc_route(&scopes, routes::friend::APPLY));
        assert!(!allows_rpc_route(&scopes, routes::friend::ACCEPT));
        // 群创建与管理
        assert!(!allows_rpc_route(&scopes, routes::group::CREATE));
        assert!(!allows_rpc_route(&scopes, routes::group_member::ADD));
        // 自助建单聊
        assert!(!allows_rpc_route(
            &scopes,
            routes::channel::DIRECT_GET_OR_CREATE
        ));
        // 身份扩散
        assert!(!allows_rpc_route(&scopes, routes::account_user::SHARE_CARD));
        assert!(!allows_rpc_route(&scopes, routes::user_qrcode::GET));
    }

    #[test]
    fn messaging_scope_denies_non_messaging_message_types() {
        let scopes = messaging();
        assert!(!allows_message_type(&scopes, &MessageType::SubscribeRequest));
        assert!(!allows_message_type(&scopes, &MessageType::PublishRequest));
        assert!(!allows_message_type(&scopes, &MessageType::TransferRequest));
    }

    #[test]
    fn multi_scope_takes_the_union() {
        let scopes = vec![SCOPE_MESSAGING.to_string(), SCOPE_USER.to_string()];
        // 带完整能力 scope 时按并集放行
        assert!(allows_rpc_route(&scopes, routes::group::CREATE));
    }

    #[test]
    fn unknown_scope_has_no_capability() {
        for scope in ["some-future-scope", "agent", "AGENT_NODE", ""] {
            let scopes = vec![scope.to_string()];
            assert!(!allows_rpc_route(&scopes, routes::account_search::QUERY), "{scope}");
            assert!(!allows_rpc_route(&scopes, routes::auth::LOGOUT), "{scope}");
            assert!(!allows_message_type(&scopes, &MessageType::AuthorizationRequest), "{scope}");
            assert!(!allows_message_type(&scopes, &MessageType::SendMessageRequest), "{scope}");
        }
    }

    #[test]
    fn agent_node_scope_only_keeps_connection_subscribe_and_transfer() {
        let scopes = vec![SCOPE_AGENT_NODE.to_string()];
        for t in [
            MessageType::AuthorizationRequest,
            MessageType::PingRequest,
            MessageType::SubscribeRequest,
            MessageType::TransferRequest,
            MessageType::DisconnectRequest,
        ] {
            assert!(allows_message_type(&scopes, &t), "{t:?}");
        }
        for t in [MessageType::SendMessageRequest, MessageType::PublishRequest] {
            assert!(!allows_message_type(&scopes, &t), "{t:?}");
        }
        assert!(allows_rpc_route(&scopes, routes::auth::LOGOUT));
        // 读聊天、改资料、建关系、续期一律拒绝
        for route in [
            routes::sync::GET_DIFFERENCE,
            routes::message_history::GET,
            routes::entity::SYNC_ENTITIES,
            routes::account_profile::UPDATE,
            routes::account_user::DETAIL,
            routes::account_search::QUERY,
            routes::friend::APPLY,
            routes::channel::DIRECT_GET_OR_CREATE,
            routes::group::CREATE,
            routes::file::REQUEST_UPLOAD_TOKEN,
            routes::auth::REFRESH,
        ] {
            assert!(!allows_rpc_route(&scopes, route), "{route}");
        }
    }

    #[test]
    fn unknown_restricted_scope_lists_no_routes() {
        assert!(list_scope_routes("nope").is_empty());
        assert!(!list_scope_routes(SCOPE_MESSAGING).is_empty());
    }
}
