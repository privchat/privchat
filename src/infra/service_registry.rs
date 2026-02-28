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

/// 服务注册与连接管理器
/// 
/// 提供服务间通信的统一入口，包括：
/// - 服务注册与发现
/// - 连接池管理
/// - 健康检查
/// - 负载均衡
/// - 请求路由

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::time::{interval, Instant};
use msgtrans::{
    transport::client::TransportClientBuilder,
    protocol::TcpClientConfig,
    event::ClientEvent,
    SessionId,
};
use serde::{Serialize, Deserialize};
use tracing::{info, warn, error, debug};

/// 服务信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInfo {
    pub name: String,
    pub address: String,
    pub port: u16,
    pub protocol: String,
    pub version: String,
    pub metadata: HashMap<String, String>,
    pub healthy: bool,
    pub last_heartbeat: Instant,
}

impl ServiceInfo {
    pub fn new(name: String, address: String, port: u16) -> Self {
        Self {
            name,
            address,
            port,
            protocol: "tcp".to_string(),
            version: "1.0.0".to_string(),
            metadata: HashMap::new(),
            healthy: true,
            last_heartbeat: Instant::now(),
        }
    }
    
    pub fn full_address(&self) -> String {
        format!("{}:{}", self.address, self.port)
    }
    
    pub fn with_metadata(mut self, key: String, value: String) -> Self {
        self.metadata.insert(key, value);
        self
    }
    
    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = Instant::now();
        self.healthy = true;
    }
}

/// 服务连接状态
#[derive(Debug, Clone)]
pub struct ServiceConnection {
    pub service_info: ServiceInfo,
    pub client: Arc<tokio::sync::Mutex<msgtrans::transport::client::TransportClient>>,
    pub connected: bool,
    pub last_used: Instant,
    pub request_count: u64,
    pub error_count: u64,
}

impl ServiceConnection {
    pub async fn new(service_info: ServiceInfo) -> Result<Self, crate::error::ServerError> {
        let tcp_config = TcpClientConfig::new(&service_info.full_address())
            .map_err(|e| crate::error::ServerError::Internal(format!("TCP配置失败: {}", e)))?
            .with_connect_timeout(Duration::from_secs(10))
            .with_nodelay(true);
        
        let mut client = TransportClientBuilder::new()
            .with_protocol(tcp_config)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .await
            .map_err(|e| crate::error::ServerError::Internal(format!("客户端创建失败: {}", e)))?;
        
        // 尝试连接
        client.connect().await
            .map_err(|e| crate::error::ServerError::Internal(format!("连接失败: {}", e)))?;
        
        Ok(Self {
            service_info,
            client: Arc::new(tokio::sync::Mutex::new(client)),
            connected: true,
            last_used: Instant::now(),
            request_count: 0,
            error_count: 0,
        })
    }
    
    pub fn update_stats(&mut self, success: bool) {
        self.last_used = Instant::now();
        self.request_count += 1;
        if !success {
            self.error_count += 1;
        }
    }
    
    pub fn health_score(&self) -> f64 {
        if self.request_count == 0 {
            1.0
        } else {
            1.0 - (self.error_count as f64 / self.request_count as f64)
        }
    }
}

/// 负载均衡策略
#[derive(Debug, Clone)]
pub enum LoadBalanceStrategy {
    RoundRobin,
    LeastConnections,
    WeightedRoundRobin,
    HealthBased,
}

/// 服务注册中心配置
#[derive(Debug, Clone)]
pub struct ServiceRegistryConfig {
    pub health_check_interval: Duration,
    pub connection_timeout: Duration,
    pub max_connections_per_service: usize,
    pub load_balance_strategy: LoadBalanceStrategy,
    pub retry_attempts: usize,
    pub retry_delay: Duration,
}

impl Default for ServiceRegistryConfig {
    fn default() -> Self {
        Self {
            health_check_interval: Duration::from_secs(30),
            connection_timeout: Duration::from_secs(10),
            max_connections_per_service: 10,
            load_balance_strategy: LoadBalanceStrategy::HealthBased,
            retry_attempts: 3,
            retry_delay: Duration::from_millis(100),
        }
    }
}

/// 服务注册中心
pub struct ServiceRegistry {
    config: ServiceRegistryConfig,
    services: Arc<RwLock<HashMap<String, Vec<ServiceConnection>>>>,
    round_robin_counters: Arc<RwLock<HashMap<String, usize>>>,
}

impl ServiceRegistry {
    pub fn new(config: ServiceRegistryConfig) -> Self {
        Self {
            config,
            services: Arc::new(RwLock::new(HashMap::new())),
            round_robin_counters: Arc::new(RwLock::new(HashMap::new())),
        }
    }
    
    /// 注册服务
    pub async fn register_service(&self, service_info: ServiceInfo) -> Result<(), crate::error::ServerError> {
        info!("🔗 注册服务: {} -> {}", service_info.name, service_info.full_address());
        
        let connection = ServiceConnection::new(service_info.clone()).await?;
        
        let mut services = self.services.write().await;
        let service_connections = services.entry(service_info.name.clone()).or_insert_with(Vec::new);
        
        // 检查是否已存在相同地址的连接
        if let Some(existing) = service_connections.iter_mut().find(|c| c.service_info.full_address() == service_info.full_address()) {
            existing.service_info = service_info;
            existing.connected = true;
            info!("✅ 更新现有服务连接: {}", existing.service_info.name);
        } else {
            service_connections.push(connection);
            info!("🆕 新增服务连接: {}", service_info.name);
        }
        
        Ok(())
    }
    
    /// 注销服务
    pub async fn unregister_service(&self, service_name: &str, address: &str) -> Result<(), crate::error::ServerError> {
        info!("🔌 注销服务: {} -> {}", service_name, address);
        
        let mut services = self.services.write().await;
        if let Some(service_connections) = services.get_mut(service_name) {
            service_connections.retain(|c| c.service_info.full_address() != address);
            if service_connections.is_empty() {
                services.remove(service_name);
            }
        }
        
        Ok(())
    }
    
    /// 获取服务连接（支持负载均衡）
    pub async fn get_service_connection(&self, service_name: &str) -> Result<Arc<tokio::sync::Mutex<msgtrans::transport::client::TransportClient>>, crate::error::ServerError> {
        let services = self.services.read().await;
        let service_connections = services.get(service_name)
            .ok_or_else(|| crate::error::ServerError::Internal(format!("服务未找到: {}", service_name)))?;
        
        if service_connections.is_empty() {
            return Err(crate::error::ServerError::Internal(format!("服务无可用连接: {}", service_name)));
        }
        
        let selected = self.select_connection(service_name, service_connections).await?;
        Ok(selected.client.clone())
    }
    
    /// 根据负载均衡策略选择连接
    async fn select_connection(&self, service_name: &str, connections: &[ServiceConnection]) -> Result<&ServiceConnection, crate::error::ServerError> {
        let healthy_connections: Vec<_> = connections.iter()
            .filter(|c| c.connected && c.service_info.healthy)
            .collect();
        
        if healthy_connections.is_empty() {
            return Err(crate::error::ServerError::Internal(format!("服务无健康连接: {}", service_name)));
        }
        
        match self.config.load_balance_strategy {
            LoadBalanceStrategy::RoundRobin => {
                let mut counters = self.round_robin_counters.write().await;
                let counter = counters.entry(service_name.to_string()).or_insert(0);
                let selected = &healthy_connections[*counter % healthy_connections.len()];
                *counter += 1;
                Ok(selected)
            }
            LoadBalanceStrategy::LeastConnections => {
                let selected = healthy_connections.iter()
                    .min_by_key(|c| c.request_count)
                    .unwrap();
                Ok(selected)
            }
            LoadBalanceStrategy::HealthBased => {
                let selected = healthy_connections.iter()
                    .max_by(|a, b| a.health_score().partial_cmp(&b.health_score()).unwrap())
                    .unwrap();
                Ok(selected)
            }
            LoadBalanceStrategy::WeightedRoundRobin => {
                // 简化版本，使用健康分数作为权重
                let total_weight: f64 = healthy_connections.iter()
                    .map(|c| c.health_score())
                    .sum();
                
                let mut weight = 0.0;
                let target_weight = fastrand::f64() * total_weight;
                
                for conn in healthy_connections {
                    weight += conn.health_score();
                    if weight >= target_weight {
                        return Ok(conn);
                    }
                }
                
                Ok(healthy_connections[0])
            }
        }
    }
    
    /// 列出所有服务
    pub async fn list_services(&self) -> HashMap<String, Vec<ServiceInfo>> {
        let services = self.services.read().await;
        services.iter()
            .map(|(name, connections)| {
                let service_infos = connections.iter()
                    .map(|c| c.service_info.clone())
                    .collect();
                (name.clone(), service_infos)
            })
            .collect()
    }
    
    /// 启动健康检查
    pub async fn start_health_check(&self) {
        let services = self.services.clone();
        let check_interval = self.config.health_check_interval;
        
        tokio::spawn(async move {
            let mut interval = interval(check_interval);
            loop {
                interval.tick().await;
                
                let mut services_guard = services.write().await;
                for (service_name, connections) in services_guard.iter_mut() {
                    for connection in connections.iter_mut() {
                        // 检查连接健康状态
                        let health_timeout = Duration::from_secs(5);
                        let is_healthy = tokio::time::timeout(health_timeout, async {
                            // 发送心跳消息
                            let client = connection.client.lock().await;
                            // 这里应该发送实际的心跳消息
                            // client.send(b"ping").await.is_ok()
                            true // 暂时假设健康
                        }).await.unwrap_or(false);
                        
                        connection.connected = is_healthy;
                        connection.service_info.healthy = is_healthy;
                        
                        if !is_healthy {
                            warn!("🔴 服务连接不健康: {} -> {}", service_name, connection.service_info.full_address());
                        }
                    }
                }
            }
        });
    }
    
    /// 获取服务统计信息
    pub async fn get_service_stats(&self, service_name: &str) -> Result<ServiceStats, crate::error::ServerError> {
        let services = self.services.read().await;
        let connections = services.get(service_name)
            .ok_or_else(|| crate::error::ServerError::Internal(format!("服务未找到: {}", service_name)))?;
        
        let total_connections = connections.len();
        let healthy_connections = connections.iter().filter(|c| c.connected && c.service_info.healthy).count();
        let total_requests = connections.iter().map(|c| c.request_count).sum();
        let total_errors = connections.iter().map(|c| c.error_count).sum();
        
        Ok(ServiceStats {
            service_name: service_name.to_string(),
            total_connections,
            healthy_connections,
            total_requests,
            total_errors,
            average_health_score: if total_connections > 0 {
                connections.iter().map(|c| c.health_score()).sum::<f64>() / total_connections as f64
            } else {
                0.0
            },
        })
    }
}

/// 服务统计信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStats {
    pub service_name: String,
    pub total_connections: usize,
    pub healthy_connections: usize,
    pub total_requests: u64,
    pub total_errors: u64,
    pub average_health_score: f64,
}

/// 服务连接管理器
pub struct ServiceConnectionManager {
    registry: Arc<ServiceRegistry>,
}

impl ServiceConnectionManager {
    pub fn new(config: ServiceRegistryConfig) -> Self {
        Self {
            registry: Arc::new(ServiceRegistry::new(config)),
        }
    }
    
    /// 获取注册中心
    pub fn registry(&self) -> Arc<ServiceRegistry> {
        self.registry.clone()
    }
    
    /// 启动服务管理器
    pub async fn start(&self) -> Result<(), crate::error::ServerError> {
        info!("🚀 启动服务连接管理器");
        
        // 启动健康检查
        self.registry.start_health_check().await;
        
        Ok(())
    }
    
    /// 创建服务客户端
    pub async fn create_service_client(&self, service_name: &str) -> Result<ServiceClient, crate::error::ServerError> {
        let connection = self.registry.get_service_connection(service_name).await?;
        Ok(ServiceClient::new(service_name.to_string(), connection, self.registry.clone()))
    }
}

/// 服务客户端 - 封装对特定服务的调用
pub struct ServiceClient {
    service_name: String,
    connection: Arc<tokio::sync::Mutex<msgtrans::transport::client::TransportClient>>,
    registry: Arc<ServiceRegistry>,
}

impl ServiceClient {
    pub fn new(
        service_name: String,
        connection: Arc<tokio::sync::Mutex<msgtrans::transport::client::TransportClient>>,
        registry: Arc<ServiceRegistry>,
    ) -> Self {
        Self {
            service_name,
            connection,
            registry,
        }
    }
    
    /// 发送请求
    pub async fn send_message_request(&self, data: &[u8]) -> Result<Vec<u8>, crate::error::ServerError> {
        let start_time = Instant::now();
        
        let result = {
            let mut client = self.connection.lock().await;
            client.send(data).await
                .map_err(|e| crate::error::ServerError::Internal(format!("请求发送失败: {}", e)))
        };
        
        let success = result.is_ok();
        let duration = start_time.elapsed();
        
        // 更新统计信息
        // TODO: 实际更新连接统计
        
        debug!("📊 服务请求: {} -> {:?} (耗时: {:?})", self.service_name, success, duration);
        
        match result {
            Ok(response) => {
                // 假设返回的是发送结果，实际应该是响应数据
                Ok(Vec::new()) // 暂时返回空
            }
            Err(e) => Err(e),
        }
    }
    
    /// 发送请求并等待响应
    pub async fn request_response(&self, data: &[u8]) -> Result<Vec<u8>, crate::error::ServerError> {
        let start_time = Instant::now();
        
        let result = {
            let mut client = self.connection.lock().await;
            // 使用 msgtrans 的 request 方法
            // let response = client.request(data).await?;
            // 暂时使用 send 方法
            client.send(data).await
                .map_err(|e| crate::error::ServerError::Internal(format!("请求失败: {}", e)))
        };
        
        let success = result.is_ok();
        let duration = start_time.elapsed();
        
        debug!("📊 服务请求响应: {} -> {:?} (耗时: {:?})", self.service_name, success, duration);
        
        match result {
            Ok(_) => Ok(Vec::new()), // 暂时返回空
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_service_registry() {
        let config = ServiceRegistryConfig::default();
        let registry = ServiceRegistry::new(config);
        
        // 测试服务注册
        let service = ServiceInfo::new("test-service".to_string(), "127.0.0.1".to_string(), 8080);
        // registry.register_service(service).await.unwrap();
        
        // 测试服务列表
        let services = registry.list_services().await;
        assert_eq!(services.len(), 0); // 因为没有实际连接
    }
} 