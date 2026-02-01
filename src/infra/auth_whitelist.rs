use std::collections::HashSet;
use lazy_static::lazy_static;
use privchat_protocol::message::MessageType;

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
    /// 注意：本 IM 系统支持两种账号体系模式：
    /// 1. 内置账号系统：使用服务器内置的注册/登录功能（适合独立部署）
    /// 2. 外部账号系统：认证 token 由外部业务系统签发（适合企业集成）
    /// 
    /// 通过配置 `use_internal_auth` 可以控制是否启用内置账号系统
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
        assert!(is_anonymous_message_type(&MessageType::AuthorizationRequest));
        
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
        
        // 其他所有 RPC 都不应该在白名单中
        assert!(!is_anonymous_rpc_route("account/auth/login"));
        assert!(!is_anonymous_rpc_route("account/user/register"));
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

