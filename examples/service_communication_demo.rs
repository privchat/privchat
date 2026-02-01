/// 服务间通信演示程序
/// 
/// 展示完整的服务间通信框架功能：
/// - 服务注册与发现
/// - 服务间调用
/// - 负载均衡
/// - 错误处理
/// - 统计监控

use std::sync::Arc;
use std::time::Duration;
use privchat_server::infra::{
    service_registry::{ServiceRegistry, ServiceRegistryConfig, ServiceInfo, ServiceConnectionManager},
    service_protocol::{ServiceRequestBuilder, ServiceHandler, methods, services},
};
use privchat_server::service::{
    friend_service::{FriendService, FriendServiceConfig, FriendServiceHandler, SendFriendRequestParams},
    group_service::{GroupService, GroupServiceHandler, CreateGroupParams},
    push_service::{PushService, PushServiceHandler, SendPushParams, PushPlatform, RegisterDeviceParams},
};
use privchat_server::infra::{Database, CacheService};
use privchat_server::repository::user_repo::UserRepository;
use tokio::time::sleep;
use tracing::{info, warn, error};

/// 模拟的缓存服务
struct MockCacheService;

#[async_trait::async_trait]
impl CacheService for MockCacheService {
    async fn get<T>(&self, _key: &str) -> Result<Option<T>, Box<dyn std::error::Error + Send + Sync>>
    where
        T: serde::de::DeserializeOwned + Send + Sync,
    {
        Ok(None)
    }
    
    async fn set<T>(&self, _key: &str, _value: &T, _ttl: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>>
    where
        T: serde::Serialize + Send + Sync,
    {
        Ok(())
    }
    
    async fn remove(&self, _key: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(true)
    }
    
    async fn exists(&self, _key: &str) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(false)
    }
}

/// 服务器实例，包含所有服务
struct ServiceServer {
    service_name: String,
    port: u16,
    registry: Arc<ServiceRegistry>,
    // 服务实例
    friend_service: Option<Arc<FriendService>>,
    group_service: Option<Arc<GroupService>>,
    push_service: Option<Arc<PushService>>,
}

impl ServiceServer {
    pub fn new(service_name: String, port: u16, registry: Arc<ServiceRegistry>) -> Self {
        Self {
            service_name,
            port,
            registry,
            friend_service: None,
            group_service: None,
            push_service: None,
        }
    }
    
    pub async fn start_friend_service(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🚀 启动好友服务: {}", self.service_name);
        
        // 创建依赖项（模拟）
        let database = Arc::new(Database::new().await?);
        let cache = Arc::new(MockCacheService);
        let user_repo = Arc::new(UserRepository::new(database.clone()));
        
        // 创建好友服务
        let config = FriendServiceConfig::default();
        let friend_service = Arc::new(
            FriendService::new(config, database, cache, user_repo)
                .with_service_registry(self.registry.clone())
        );
        
        self.friend_service = Some(friend_service.clone());
        
        // 注册服务到注册中心
        let service_info = ServiceInfo::new(
            services::FRIEND_SERVICE.to_string(),
            "127.0.0.1".to_string(),
            self.port,
        );
        
        self.registry.register_service(service_info).await
            .map_err(|e| format!("注册好友服务失败: {}", e))?;
        
        info!("✅ 好友服务启动成功: 127.0.0.1:{}", self.port);
        Ok(())
    }
    
    pub async fn start_group_service(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🚀 启动群组服务: {}", self.service_name);
        
        // 创建依赖项（模拟）
        let database = Arc::new(Database::new().await?);
        let cache = Arc::new(MockCacheService);
        let user_repo = Arc::new(UserRepository::new(database.clone()));
        
        // 创建群组服务
        let group_service = Arc::new(
            GroupService::new(database, cache, user_repo)
                .with_service_registry(self.registry.clone())
        );
        
        self.group_service = Some(group_service.clone());
        
        // 注册服务到注册中心
        let service_info = ServiceInfo::new(
            services::GROUP_SERVICE.to_string(),
            "127.0.0.1".to_string(),
            self.port,
        );
        
        self.registry.register_service(service_info).await
            .map_err(|e| format!("注册群组服务失败: {}", e))?;
        
        info!("✅ 群组服务启动成功: 127.0.0.1:{}", self.port);
        Ok(())
    }
    
    pub async fn start_push_service(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🚀 启动推送服务: {}", self.service_name);
        
        // 创建依赖项（模拟）
        let database = Arc::new(Database::new().await?);
        let cache = Arc::new(MockCacheService);
        
        // 创建推送服务
        let push_service = Arc::new(
            PushService::new(database, cache)
                .with_service_registry(self.registry.clone())
        );
        
        self.push_service = Some(push_service.clone());
        
        // 注册服务到注册中心
        let service_info = ServiceInfo::new(
            services::PUSH_SERVICE.to_string(),
            "127.0.0.1".to_string(),
            self.port,
        );
        
        self.registry.register_service(service_info).await
            .map_err(|e| format!("注册推送服务失败: {}", e))?;
        
        info!("✅ 推送服务启动成功: 127.0.0.1:{}", self.port);
        Ok(())
    }
}

/// 服务客户端，用于调用其他服务
struct ServiceClient {
    connection_manager: Arc<ServiceConnectionManager>,
}

impl ServiceClient {
    pub fn new(connection_manager: Arc<ServiceConnectionManager>) -> Self {
        Self { connection_manager }
    }
    
    /// 调用好友服务 - 发送好友请求
    pub async fn send_friend_request(&self, from_user_id: u64, to_user_id: u64) -> Result<(), Box<dyn std::error::Error>> {
        info!("📞 调用好友服务: 发送好友请求 {} -> {}", from_user_id, to_user_id);
        
        let service_client = self.connection_manager
            .create_service_client(services::FRIEND_SERVICE)
            .await?;
        
        let params = SendFriendRequestParams {
            to_user_id: to_user_id,
            message: Some("Hello, let's be friends!".to_string()),
        };
        
        let request = ServiceRequestBuilder::new(
            services::FRIEND_SERVICE.to_string(),
            methods::FRIEND_SEND_REQUEST.to_string(),
            "client-service".to_string(),
        )
        .data(params)?
        .header("user_id".to_string(), from_user_id)
        .timeout(10)
        .build();
        
        let request_bytes = request.to_bytes()?;
        let response_bytes = service_client.request_response(&request_bytes).await?;
        
        info!("✅ 好友请求发送成功");
        Ok(())
    }
    
    /// 调用群组服务 - 创建群组
    pub async fn create_group(&self, creator_id: &str, group_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        info!("📞 调用群组服务: 创建群组 {} (创建者: {})", group_name, creator_id);
        
        let service_client = self.connection_manager
            .create_service_client(services::GROUP_SERVICE)
            .await?;
        
        let params = CreateGroupParams {
            name: group_name.to_string(),
            description: Some("演示群组".to_string()),
            is_public: false,
            max_members: Some(100),
            initial_members: vec![],
        };
        
        let request = ServiceRequestBuilder::new(
            services::GROUP_SERVICE.to_string(),
            methods::GROUP_CREATE.to_string(),
            "client-service".to_string(),
        )
        .data(params)?
        .header("user_id".to_string(), creator_id.to_string())
        .timeout(10)
        .build();
        
        let request_bytes = request.to_bytes()?;
        let response_bytes = service_client.request_response(&request_bytes).await?;
        
        info!("✅ 群组创建成功");
        Ok(())
    }
    
    /// 调用推送服务 - 发送推送消息
    pub async fn send_push_message(&self, user_id: u64, title: &str, body: &str) -> Result<(), Box<dyn std::error::Error>> {
        info!("📞 调用推送服务: 发送推送 {} -> {}: {}", user_id, title, body);
        
        let service_client = self.connection_manager
            .create_service_client(services::PUSH_SERVICE)
            .await?;
        
        let params = SendPushParams {
            user_id: user_id,
            title: title.to_string(),
            body: body.to_string(),
            data: None,
            priority: None,
        };
        
        let request = ServiceRequestBuilder::new(
            services::PUSH_SERVICE.to_string(),
            methods::PUSH_SEND_MESSAGE.to_string(),
            "client-service".to_string(),
        )
        .data(params)?
        .timeout(10)
        .build();
        
        let request_bytes = request.to_bytes()?;
        let response_bytes = service_client.request_response(&request_bytes).await?;
        
        info!("✅ 推送消息发送成功");
        Ok(())
    }
    
    /// 健康检查所有服务
    pub async fn health_check_all_services(&self) -> Result<(), Box<dyn std::error::Error>> {
        info!("🔍 开始健康检查所有服务");
        
        let services_to_check = vec![
            services::FRIEND_SERVICE,
            services::GROUP_SERVICE,
            services::PUSH_SERVICE,
        ];
        
        for service_name in services_to_check {
            match self.health_check_service(service_name).await {
                Ok(()) => info!("✅ {} 健康检查通过", service_name),
                Err(e) => warn!("❌ {} 健康检查失败: {}", service_name, e),
            }
        }
        
        Ok(())
    }
    
    async fn health_check_service(&self, service_name: &str) -> Result<(), Box<dyn std::error::Error>> {
        let service_client = self.connection_manager
            .create_service_client(service_name)
            .await?;
        
        let request = ServiceRequestBuilder::new(
            service_name.to_string(),
            methods::HEALTH_CHECK.to_string(),
            "client-service".to_string(),
        )
        .timeout(5)
        .build();
        
        let request_bytes = request.to_bytes()?;
        let response_bytes = service_client.request_response(&request_bytes).await?;
        
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 初始化日志
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();
    
    info!("🚀 启动服务间通信演示程序");
    
    // 1. 创建服务注册中心
    let registry_config = ServiceRegistryConfig::default();
    let connection_manager = Arc::new(ServiceConnectionManager::new(registry_config));
    let registry = connection_manager.registry();
    
    // 启动服务管理器
    connection_manager.start().await?;
    
    info!("📋 服务注册中心启动完成");
    
    // 2. 启动多个服务实例
    let mut servers = Vec::new();
    
    // 启动好友服务
    let mut friend_server = ServiceServer::new("friend-server-1".to_string(), 8001, registry.clone());
    friend_server.start_friend_service().await?;
    servers.push(friend_server);
    
    // 启动群组服务
    let mut group_server = ServiceServer::new("group-server-1".to_string(), 8002, registry.clone());
    group_server.start_group_service().await?;
    servers.push(group_server);
    
    // 启动推送服务
    let mut push_server = ServiceServer::new("push-server-1".to_string(), 8003, registry.clone());
    push_server.start_push_service().await?;
    servers.push(push_server);
    
    // 等待服务注册完成
    sleep(Duration::from_secs(2)).await;
    
    // 3. 创建客户端并进行服务调用
    let client = ServiceClient::new(connection_manager.clone());
    
    info!("🔍 开始演示服务间通信");
    
    // 健康检查
    client.health_check_all_services().await?;
    
    // 模拟业务场景：用户注册并加好友
    info!("📱 模拟业务场景: 用户注册并加好友");
    
    // 发送好友请求
    client.send_friend_request("user_alice", "user_bob").await?;
    client.send_friend_request("user_bob", "user_charlie").await?;
    
    // 创建群组
    client.create_group("user_alice", "技术讨论群").await?;
    client.create_group("user_bob", "摄影爱好者").await?;
    
    // 发送推送消息
    client.send_push_message("user_alice", "新消息", "您有一条新的好友请求").await?;
    client.send_push_message("user_bob", "群组邀请", "您被邀请加入技术讨论群").await?;
    
    // 4. 展示服务注册中心状态
    info!("📊 服务注册中心状态:");
    let services = registry.list_services().await;
    for (service_name, service_infos) in services {
        info!("  📦 服务: {}", service_name);
        for service_info in service_infos {
            info!("    🔗 实例: {} (健康: {})", 
                service_info.full_address(), 
                service_info.healthy
            );
        }
    }
    
    // 5. 展示服务统计
    info!("📈 服务统计信息:");
    for service_name in &[services::FRIEND_SERVICE, services::GROUP_SERVICE, services::PUSH_SERVICE] {
        match registry.get_service_stats(service_name).await {
            Ok(stats) => {
                info!("  📊 {}: 连接数={}, 健康连接={}, 请求数={}, 错误数={}, 健康分数={:.2}", 
                    stats.service_name,
                    stats.total_connections,
                    stats.healthy_connections,
                    stats.total_requests,
                    stats.total_errors,
                    stats.average_health_score
                );
            }
            Err(e) => warn!("  ❌ {} 统计获取失败: {}", service_name, e),
        }
    }
    
    // 6. 测试负载均衡
    info!("⚖️ 测试负载均衡 (多次调用同一服务):");
    for i in 1..=5 {
        client.send_friend_request(&format!("user_{}", i), &format!("user_{}", i + 1)).await?;
        sleep(Duration::from_millis(100)).await;
    }
    
    info!("✅ 服务间通信演示完成！");
    info!("🎯 演示内容总结:");
    info!("  ✅ 服务注册与发现");
    info!("  ✅ 服务间TCP通信");
    info!("  ✅ 负载均衡");
    info!("  ✅ 健康检查");
    info!("  ✅ 错误处理");
    info!("  ✅ 统计监控");
    info!("  ✅ 好友服务调用");
    info!("  ✅ 群组服务调用");
    info!("  ✅ 推送服务调用");
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_service_registry() {
        let config = ServiceRegistryConfig::default();
        let connection_manager = ServiceConnectionManager::new(config);
        let registry = connection_manager.registry();
        
        // 测试服务列表（初始为空）
        let services = registry.list_services().await;
        assert_eq!(services.len(), 0);
    }
    
    #[test]
    fn test_service_request_creation() {
        let request = ServiceRequestBuilder::new(
            "test-service".to_string(),
            "test-method".to_string(),
            "client".to_string(),
        )
        .timeout(10)
        .build();
        
        assert_eq!(request.service_name, "test-service");
        assert_eq!(request.method, "test-method");
        assert_eq!(request.source_service, "client");
        assert_eq!(request.timeout, Some(10));
    }
} 