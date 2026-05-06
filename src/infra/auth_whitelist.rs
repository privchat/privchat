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

use lazy_static::lazy_static;
use privchat_protocol::protocol::MessageType;
use std::collections::HashSet;

lazy_static! {
    /// 匿名消息类型白名单
    ///
    /// 这些消息类型可以在未认证的情况下访问
    pub static ref ANONYMOUS_MESSAGE_TYPES: HashSet<MessageType> = {
        let mut set = HashSet::new();

        // Connect 消息必须匿名（这是认证的入口）
        set.insert(MessageType::AuthorizationRequest);

        set
    };

    /// 匿名 RPC 路由白名单
    ///
    /// 这些 RPC 接口可以在未认证的情况下访问
    ///
    /// 注意：本 IM 系统支持两种账号体系模式（见配置 `[account] mode`）：
    /// 1. BUILTIN：使用服务器内置的注册/登录功能（适合独立部署）
    /// 2. PLATFORM：认证 token 由 privchat platform（外部）签发（适合企业集成）
    ///
    /// `account/auth/login` 与 `account/user/register` 仍出现在白名单（路由可达），
    /// PLATFORM 模式下由 handler 返 forbidden；spec 接受这种语义（避免动态白名单复杂度）。
    pub static ref ANONYMOUS_RPC_ROUTES: HashSet<String> = {
        let mut set = HashSet::new();

        // ==================== 系统信息（公开访问） ====================
        // 健康检查 - 用于监控和负载均衡
        set.insert("system/health".to_string());

        // 系统信息 - 版本号、能力列表等
        set.insert("system/info".to_string());

        // ==================== 账号认证（可配置） ====================
        // 用户登录 - 返回 JWT token，可直接用于 AuthorizationRequest
        set.insert("account/auth/login".to_string());

        // 用户注册 - 注册成功后返回 JWT token，可直接用于 AuthorizationRequest
        set.insert("account/user/register".to_string());

        // 刷新 access token - refresh_token 自带 JWT 签名校验，不需要已认证 IM session
        // （spec TOKEN_REFRESH_SPEC §8.1：白名单接口；由 client 在 access token 过期、
        // ConnAuth 拿到 10002 后调用，回到 IM 通道继续 authenticate）
        set.insert("account/auth/refresh".to_string());

        // ==================== 扫码登录（spec QR_API §5） ====================
        // Web/PC unauth 连接创建二维码 scene；server 内部把 scene_id ↔ session_id
        // 绑定到 QrLoginPublisher，后续 scanned/authorized/rejected/expired 推回这条连接。
        set.insert("qr_login/create_scene".to_string());

        set
    };
}

/// 检查消息类型是否允许匿名访问
///
/// # 参数
/// - msg_type: 消息类型
///
/// # 返回
/// - true: 允许匿名访问（在白名单中）
/// - false: 需要认证
pub fn is_anonymous_message_type(msg_type: &MessageType) -> bool {
    ANONYMOUS_MESSAGE_TYPES.contains(msg_type)
}

/// 检查 RPC 路由是否允许匿名访问
///
/// # 参数
/// - route: RPC 路由，如 "account/auth/login"
///
/// # 返回
/// - true: 允许匿名访问（在白名单中）
/// - false: 需要认证
pub fn is_anonymous_rpc_route(route: &str) -> bool {
    ANONYMOUS_RPC_ROUTES.contains(route)
}

/// 获取所有匿名消息类型
pub fn list_anonymous_message_types() -> Vec<MessageType> {
    ANONYMOUS_MESSAGE_TYPES.iter().cloned().collect()
}

/// 获取所有匿名 RPC 路由
pub fn list_anonymous_rpc_routes() -> Vec<String> {
    ANONYMOUS_RPC_ROUTES.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anonymous_message_types() {
        // AuthorizationRequest 消息应该在白名单中
        assert!(is_anonymous_message_type(
            &MessageType::AuthorizationRequest
        ));

        // SendMessageRequest 消息不应该在白名单中
        assert!(!is_anonymous_message_type(&MessageType::SendMessageRequest));

        // PushMessageRequest 消息不应该在白名单中
        assert!(!is_anonymous_message_type(&MessageType::PushMessageRequest));
    }

    #[test]
    fn test_anonymous_rpc_routes() {
        // 系统信息应该在白名单中
        assert!(is_anonymous_rpc_route("system/health"));
        assert!(is_anonymous_rpc_route("system/info"));
        assert!(is_anonymous_rpc_route("account/auth/login"));
        assert!(is_anonymous_rpc_route("account/user/register"));
        assert!(is_anonymous_rpc_route("account/auth/refresh"));
        assert!(is_anonymous_rpc_route("qr_login/create_scene"));

        // 其他所有 RPC 都不应该在白名单中
        assert!(!is_anonymous_rpc_route("qrcode/resolve"));
        assert!(!is_anonymous_rpc_route("message/send"));
        assert!(!is_anonymous_rpc_route("group/member/remove"));
        assert!(!is_anonymous_rpc_route("user/profile/update"));
    }

    #[test]
    fn test_list_functions() {
        let msg_types = list_anonymous_message_types();
        assert!(msg_types.contains(&MessageType::AuthorizationRequest));

        let rpc_routes = list_anonymous_rpc_routes();
        assert!(rpc_routes.contains(&"system/health".to_string()));
        assert!(rpc_routes.contains(&"system/info".to_string()));
    }
}
