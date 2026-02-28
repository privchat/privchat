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

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::Path;
use std::time::Duration;
use tracing::info;

/// 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 服务器监听地址
    pub host: String,
    /// 服务器监听端口
    pub port: u16,
    /// 数据库连接字符串
    pub database_url: String,
    /// JWT密钥
    pub jwt_secret: String,
    /// 最大连接数
    pub max_connections: u32,
    /// 连接超时时间（秒）
    pub connection_timeout: u64,
    /// 心跳间隔（秒）
    pub heartbeat_interval: u64,
    /// 日志级别
    pub log_level: String,
    /// 是否启用TLS
    pub enable_tls: bool,
    /// TLS证书文件路径
    pub tls_cert_path: Option<String>,
    /// TLS私钥文件路径
    pub tls_key_path: Option<String>,
    /// 缓存配置
    pub cache: CacheConfig,
    /// 启用的协议
    pub enabled_protocols: Vec<String>,
    /// TCP 监听地址（由 gateway_listeners 中首个 tcp 推导，供当前 msgtrans 单协议单地址使用）
    pub tcp_bind_address: String,
    /// WebSocket 监听地址
    pub websocket_bind_address: String,
    /// QUIC 监听地址
    pub quic_bind_address: String,
    /// 网关多监听入口（listeners 数组，生产级可扩展；未来可多实例/多协议多地址）
    pub gateway_listeners: Vec<GatewayListenerConfig>,
    /// 存储源列表（必须至少配置一个 [[file.storage_sources]]）
    pub file_storage_sources: Vec<FileStorageSourceConfig>,
    /// 默认存储源 ID（上传时使用，须在 file_storage_sources 中存在）
    pub file_default_storage_source_id: u32,
    /// HTTP 文件服务器端口（用于启动服务）
    pub http_file_server_port: u16,
    /// 管理 API 服务器端口（仅内网访问）
    pub admin_api_port: u16,
    /// 文件服务 API 基础 URL（用于客户端访问，不包含端口号）
    ///
    /// 文件服务的 HTTP 服务器是独立的，客户端通过此 URL 访问文件相关接口。
    /// 例如：https://files.example.com/api/app
    ///
    /// 注意：此 URL 不包含端口号，生产环境通常通过域名访问（80/443 端口）
    pub file_api_base_url: Option<String>,
    /// 是否启用内置账号系统
    ///
    /// - true: 使用服务器内置的注册/登录功能（适合独立部署）
    /// - false: 使用外部账号系统（适合企业集成，token 由外部系统签发）
    pub use_internal_auth: bool,
    /// 系统消息配置
    pub system_message: SystemMessageConfig,
    /// 安全防护配置
    pub security: SecurityProtectionConfig,
    /// 业务 Handler 最大并发数（Semaphore 限流）
    /// 仅限制业务处理层，不影响连接层 read/accept
    pub handler_max_inflight: usize,
    /// Service Master Key（管理 API 认证）
    pub service_master_key: String,
    /// Redis 连接地址
    pub redis_url: String,
    /// 推送配置
    pub push: PushConfig,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9001,
            database_url: String::new(),
            jwt_secret: String::new(),
            max_connections: 1000,
            connection_timeout: 300,
            heartbeat_interval: 60,
            log_level: "info".to_string(),
            enable_tls: false,
            tls_cert_path: None,
            tls_key_path: None,
            cache: CacheConfig::default(),
            enabled_protocols: vec![
                "tcp".to_string(),
                "websocket".to_string(),
                "quic".to_string(),
            ],
            tcp_bind_address: "0.0.0.0:9001".to_string(),
            websocket_bind_address: "0.0.0.0:9080".to_string(),
            quic_bind_address: "0.0.0.0:9001".to_string(),
            gateway_listeners: default_gateway_listeners(),
            file_storage_sources: vec![],
            file_default_storage_source_id: 0,
            http_file_server_port: 9083,
            admin_api_port: 9090,
            file_api_base_url: Some("http://localhost:9083/api/app".to_string()),
            use_internal_auth: true, // 默认启用内置账号系统（方便独立部署和测试）
            system_message: SystemMessageConfig::default(),
            security: SecurityProtectionConfig::default(),
            handler_max_inflight: 2000,
            service_master_key: String::new(),
            redis_url: String::new(),
            push: PushConfig::default(),
        }
    }
}

impl ServerConfig {
    /// 创建新的服务器配置
    pub fn new() -> Self {
        Self::default()
    }

    /// 高性能服务器配置（256GB+ 内存）
    pub fn for_high_performance_server() -> Self {
        Self {
            max_connections: 10000,
            connection_timeout: 600,
            heartbeat_interval: 30,
            cache: CacheConfig {
                l1_max_memory_mb: 2048,
                l1_ttl_secs: 3600, // 1 hour
                redis: None,
                online_status: OnlineStatusConfig::default(),
            },
            ..Self::default()
        }
    }

    /// 中等性能服务器配置（64GB+ 内存）
    pub fn for_medium_performance_server() -> Self {
        Self {
            max_connections: 5000,
            connection_timeout: 450,
            heartbeat_interval: 45,
            cache: CacheConfig {
                l1_max_memory_mb: 1024,
                l1_ttl_secs: 3600, // 1 hour
                redis: None,
                online_status: OnlineStatusConfig::default(),
            },
            ..Self::default()
        }
    }

    /// 添加Redis配置
    pub fn with_redis(mut self, redis_url: String) -> Self {
        self.cache.redis = Some(RedisConfig {
            url: redis_url,
            pool_size: 50,
            min_idle: 10,
            connection_timeout_secs: 5,
            command_timeout_ms: 5000,
            idle_timeout_secs: 300,
        });
        self
    }

    /// 从 TOML 文件加载配置
    pub fn from_toml_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .with_context(|| format!("无法读取配置文件: {:?}", path.as_ref()))?;

        let toml_config: TomlConfig =
            toml::from_str(&content).with_context(|| "配置文件格式错误")?;

        Ok(toml_config.into())
    }

    /// 从环境变量加载配置（PRIVCHAT_ 前缀）
    pub fn merge_from_env(&mut self) -> Result<()> {
        fn parse_env_bool(value: &str) -> Option<bool> {
            match value.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            }
        }

        // 服务器配置
        if let Ok(host) = env::var("PRIVCHAT_HOST") {
            self.host = host;
        }
        if let Ok(port) = env::var("PRIVCHAT_PORT") {
            self.port = port.parse().unwrap_or(self.port);
        }
        if let Ok(db_url) = env::var("DATABASE_URL") {
            self.database_url = db_url;
        }
        if let Ok(jwt_secret) = env::var("PRIVCHAT_JWT_SECRET") {
            self.jwt_secret = jwt_secret;
        }
        if let Ok(max_conn) = env::var("PRIVCHAT_MAX_CONNECTIONS") {
            self.max_connections = max_conn.parse().unwrap_or(self.max_connections);
        }
        if let Ok(max_inflight) = env::var("PRIVCHAT_HANDLER_MAX_INFLIGHT") {
            self.handler_max_inflight = max_inflight.parse().unwrap_or(self.handler_max_inflight);
        }
        if let Ok(log_level) = env::var("PRIVCHAT_LOG_LEVEL") {
            self.log_level = log_level;
        }
        if let Ok(_log_format) = env::var("PRIVCHAT_LOG_FORMAT") {
            // 将在日志初始化时使用
        }

        // Service Master Key
        if let Ok(key) = env::var("SERVICE_MASTER_KEY") {
            self.service_master_key = key;
        }

        // Redis 配置
        if let Ok(redis_url) = env::var("REDIS_URL") {
            self.redis_url = redis_url.clone();
            let existing = self.cache.redis.as_ref();
            self.cache.redis = Some(RedisConfig {
                url: redis_url,
                pool_size: existing.map_or(50, |r| r.pool_size),
                min_idle: existing.map_or(10, |r| r.min_idle),
                connection_timeout_secs: existing.map_or(5, |r| r.connection_timeout_secs),
                command_timeout_ms: existing.map_or(5000, |r| r.command_timeout_ms),
                idle_timeout_secs: existing.map_or(300, |r| r.idle_timeout_secs),
            });
        }

        // 管理 API 端口
        if let Ok(admin_port) = env::var("PRIVCHAT_ADMIN_API_PORT") {
            self.admin_api_port = admin_port.parse().unwrap_or(self.admin_api_port);
        }

        // 文件配置
        if let Ok(file_api_url) = env::var("PRIVCHAT_FILE_API_BASE_URL") {
            self.file_api_base_url = Some(file_api_url);
        }

        // Push 总开关
        if let Ok(v) = env::var("PUSH_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.enabled = parsed;
            }
        }

        // APNs
        if let Ok(v) = env::var("PUSH_APNS_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.apns.enabled = parsed;
            }
        }
        if let Ok(bundle_id) = env::var("PUSH_APNS_BUNDLE_ID") {
            self.push.apns.bundle_id = Some(bundle_id);
        }
        if let Ok(team_id) = env::var("PUSH_APNS_TEAM_ID") {
            self.push.apns.team_id = Some(team_id);
        }
        if let Ok(key_id) = env::var("PUSH_APNS_KEY_ID") {
            self.push.apns.key_id = Some(key_id);
        }
        if let Ok(private_key_path) = env::var("PUSH_APNS_PRIVATE_KEY_PATH") {
            self.push.apns.private_key_path = Some(private_key_path);
        }
        if let Ok(v) = env::var("PUSH_APNS_USE_SANDBOX") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.apns.use_sandbox = parsed;
            }
        }

        // FCM
        if let Ok(v) = env::var("PUSH_FCM_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.fcm.enabled = parsed;
            }
        }
        if let Ok(project_id) = env::var("PUSH_FCM_PROJECT_ID") {
            self.push.fcm.project_id = Some(project_id);
        }
        if let Ok(access_token) = env::var("PUSH_FCM_ACCESS_TOKEN") {
            self.push.fcm.access_token = Some(access_token);
        }

        // HMS
        if let Ok(v) = env::var("PUSH_HMS_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.hms.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_HMS_APP_ID") {
            self.push.hms.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_HMS_ACCESS_TOKEN") {
            self.push.hms.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_HMS_ENDPOINT") {
            self.push.hms.endpoint = Some(endpoint);
        }

        // Honor (HMS 协议，独立凭证)
        if let Ok(v) = env::var("PUSH_HONOR_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.honor.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_HONOR_APP_ID") {
            self.push.honor.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_HONOR_ACCESS_TOKEN") {
            self.push.honor.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_HONOR_ENDPOINT") {
            self.push.honor.endpoint = Some(endpoint);
        }

        // Xiaomi
        if let Ok(v) = env::var("PUSH_XIAOMI_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.xiaomi.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_XIAOMI_APP_ID") {
            self.push.xiaomi.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_XIAOMI_ACCESS_TOKEN") {
            self.push.xiaomi.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_XIAOMI_ENDPOINT") {
            self.push.xiaomi.endpoint = Some(endpoint);
        }

        // OPPO
        if let Ok(v) = env::var("PUSH_OPPO_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.oppo.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_OPPO_APP_ID") {
            self.push.oppo.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_OPPO_ACCESS_TOKEN") {
            self.push.oppo.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_OPPO_ENDPOINT") {
            self.push.oppo.endpoint = Some(endpoint);
        }

        // Vivo
        if let Ok(v) = env::var("PUSH_VIVO_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.vivo.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_VIVO_APP_ID") {
            self.push.vivo.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_VIVO_ACCESS_TOKEN") {
            self.push.vivo.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_VIVO_ENDPOINT") {
            self.push.vivo.endpoint = Some(endpoint);
        }

        // Lenovo
        if let Ok(v) = env::var("PUSH_LENOVO_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.lenovo.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_LENOVO_APP_ID") {
            self.push.lenovo.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_LENOVO_ACCESS_TOKEN") {
            self.push.lenovo.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_LENOVO_ENDPOINT") {
            self.push.lenovo.endpoint = Some(endpoint);
        }

        // ZTE
        if let Ok(v) = env::var("PUSH_ZTE_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.zte.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_ZTE_APP_ID") {
            self.push.zte.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_ZTE_ACCESS_TOKEN") {
            self.push.zte.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_ZTE_ENDPOINT") {
            self.push.zte.endpoint = Some(endpoint);
        }

        // Meizu
        if let Ok(v) = env::var("PUSH_MEIZU_ENABLED") {
            if let Some(parsed) = parse_env_bool(&v) {
                self.push.meizu.enabled = parsed;
            }
        }
        if let Ok(app_id) = env::var("PUSH_MEIZU_APP_ID") {
            self.push.meizu.app_id = Some(app_id);
        }
        if let Ok(access_token) = env::var("PUSH_MEIZU_ACCESS_TOKEN") {
            self.push.meizu.access_token = Some(access_token);
        }
        if let Ok(endpoint) = env::var("PUSH_MEIZU_ENDPOINT") {
            self.push.meizu.endpoint = Some(endpoint);
        }

        Ok(())
    }

    /// 获取文件存储源列表（必须在 config.toml 中配置 [[file.storage_sources]]）
    pub fn effective_file_storage_sources(&self) -> Vec<FileStorageSourceConfig> {
        self.file_storage_sources.clone()
    }

    /// 从命令行参数合并配置
    pub fn merge_from_cli(&mut self, cli: &crate::cli::Cli) {
        if let Some(host) = &cli.host {
            self.host = host.clone();
        }
        if let Some(tcp_port) = cli.tcp_port {
            self.tcp_bind_address = format!("{}:{}", self.host, tcp_port);
        }
        if let Some(ws_port) = cli.ws_port {
            self.websocket_bind_address = format!("{}:{}", self.host, ws_port);
        }
        if let Some(quic_port) = cli.quic_port {
            self.quic_bind_address = format!("{}:{}", self.host, quic_port);
        }
        if let Some(max_conn) = cli.max_connections {
            self.max_connections = max_conn;
        }
        if let Some(db_url) = &cli.database_url {
            self.database_url = db_url.clone();
        }
        if let Some(redis_url) = &cli.redis_url {
            let existing = self.cache.redis.as_ref();
            self.cache.redis = Some(RedisConfig {
                url: redis_url.clone(),
                pool_size: existing.map_or(50, |r| r.pool_size),
                min_idle: existing.map_or(10, |r| r.min_idle),
                connection_timeout_secs: existing.map_or(5, |r| r.connection_timeout_secs),
                command_timeout_ms: existing.map_or(5000, |r| r.command_timeout_ms),
                idle_timeout_secs: existing.map_or(300, |r| r.idle_timeout_secs),
            });
        }
        if let Some(jwt_secret) = &cli.jwt_secret {
            self.jwt_secret = jwt_secret.clone();
        }
        if let Some(log_level) = cli.get_log_level() {
            self.log_level = log_level;
        }
    }

    /// 加载配置（按优先级：命令行 > 环境变量 > 配置文件 > 默认值）
    pub fn load(cli: &crate::cli::Cli) -> Result<Self> {
        // 1. 从默认配置开始
        let mut config = if let Some(env_str) = &cli.env {
            match env_str.as_str() {
                "production" => {
                    info!("🔧 Production 环境");
                    Self::default()
                }
                "development" | "dev" => {
                    info!("🔧 Development 环境");
                    Self::default()
                }
                _ => Self::default(),
            }
        } else if let Ok(server_mode) = env::var("SERVER_MODE") {
            match server_mode.as_str() {
                "high_performance" => {
                    info!("🔥 High Performance Mode (256GB+ Memory)");
                    Self::for_high_performance_server()
                }
                "medium_performance" => {
                    info!("⚡ Medium Performance Mode (64GB+ Memory)");
                    Self::for_medium_performance_server()
                }
                _ => {
                    info!("🔧 Default Mode");
                    Self::new()
                }
            }
        } else {
            Self::new()
        };

        // 2. 从配置文件加载（如果指定）
        if let Some(config_file) = &cli.config_file {
            if Path::new(config_file).exists() {
                info!("📄 从配置文件加载: {}", config_file);
                let file_config = Self::from_toml_file(config_file)?;
                // 合并文件配置（文件配置优先级低于环境变量和命令行）
                config = file_config;
            } else {
                tracing::warn!("⚠️ 配置文件不存在: {}", config_file);
            }
        } else if Path::new("config.toml").exists() {
            // 尝试加载默认配置文件
            info!("📄 从默认配置文件加载: config.toml");
            let file_config = Self::from_toml_file("config.toml")?;
            config = file_config;
        }

        // 3. 从环境变量合并（优先级高于配置文件）
        config.merge_from_env()?;

        // 4. 从命令行参数合并（最高优先级）
        config.merge_from_cli(cli);

        // 5. 校验必填项
        config.validate()?;

        Ok(config)
    }

    /// 校验必填配置项，缺失则报错退出
    fn validate(&self) -> Result<()> {
        let mut missing = Vec::new();

        if self.database_url.is_empty() {
            missing.push("DATABASE_URL");
        }
        if self.jwt_secret.is_empty() {
            missing.push("PRIVCHAT_JWT_SECRET");
        }
        if self.service_master_key.is_empty() {
            missing.push("SERVICE_MASTER_KEY");
        }
        if self.redis_url.is_empty() {
            missing.push("REDIS_URL");
        }

        if !missing.is_empty() {
            anyhow::bail!(
                "缺少必填环境变量: {}\n请在 .env 文件或环境变量中配置后重试",
                missing.join(", ")
            );
        }

        Ok(())
    }
}

/// TOML 配置文件结构（用于反序列化）
#[derive(Debug, Deserialize)]
struct TomlConfig {
    gateway: Option<TomlGatewayConfig>,
    cache: Option<TomlCacheConfig>,
    file: Option<TomlFileConfig>,
    admin: Option<TomlAdminConfig>,
    logging: Option<TomlLoggingConfig>,
    system_message: Option<TomlSystemMessageConfig>,
    push: Option<TomlPushConfig>,
}

/// TOML [admin] 段
#[derive(Debug, Deserialize)]
struct TomlAdminConfig {
    /// 管理 API 监听端口
    port: Option<u16>,
    /// Master Key（管理 API 认证）
    master_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushConfig {
    enabled: Option<bool>,
    apns: Option<TomlPushApnsConfig>,
    fcm: Option<TomlPushFcmConfig>,
    hms: Option<TomlPushHmsConfig>,
    honor: Option<TomlPushHonorConfig>,
    xiaomi: Option<TomlPushXiaomiConfig>,
    oppo: Option<TomlPushOppoConfig>,
    vivo: Option<TomlPushVivoConfig>,
    lenovo: Option<TomlPushLenovoConfig>,
    zte: Option<TomlPushZteConfig>,
    meizu: Option<TomlPushMeizuConfig>,
}

#[derive(Debug, Deserialize)]
struct TomlPushApnsConfig {
    enabled: Option<bool>,
    bundle_id: Option<String>,
    team_id: Option<String>,
    key_id: Option<String>,
    private_key_path: Option<String>,
    use_sandbox: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct TomlPushFcmConfig {
    enabled: Option<bool>,
    project_id: Option<String>,
    access_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushHmsConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushHonorConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushXiaomiConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushOppoConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushVivoConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushLenovoConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushZteConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlPushMeizuConfig {
    enabled: Option<bool>,
    app_id: Option<String>,
    access_token: Option<String>,
    endpoint: Option<String>,
}

/// 单条网关监听配置（listeners 数组元素，生产级可扩展）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayListenerConfig {
    /// 协议：tcp / websocket / quic（未来可扩展 http2 / grpc / unix 等）
    pub protocol: String,
    /// 监听 host
    pub host: String,
    /// 监听 port
    pub port: u16,
    /// 绑定地址（host:port），便于直接传给 msgtrans
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
    /// QUIC/TLS：证书路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_cert: Option<String>,
    /// QUIC/TLS：私钥路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_key: Option<String>,
    /// WebSocket：path，如 "/gate"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// WebSocket：是否压缩
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compression: Option<bool>,
    /// 是否内网专用（如 127.0.0.1:18080）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal: Option<bool>,
}

impl GatewayListenerConfig {
    /// 返回 bind 地址字符串
    pub fn bind_address(&self) -> String {
        self.bind_address
            .clone()
            .unwrap_or_else(|| format!("{}:{}", self.host, self.port))
    }
}

/// TOML 单条 listener 反序列化
#[derive(Debug, Deserialize)]
struct TomlListenerConfig {
    protocol: String,
    #[serde(default = "default_listener_host")]
    host: String,
    port: u16,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    path: Option<String>,
    compression: Option<bool>,
    internal: Option<bool>,
}

fn default_listener_host() -> String {
    "0.0.0.0".to_string()
}

/// 默认网关 listeners：TCP/QUIC 同端口 9001，WebSocket 单独 9080（PrivChat 端口规范）
fn default_gateway_listeners() -> Vec<GatewayListenerConfig> {
    vec![
        GatewayListenerConfig {
            protocol: "tcp".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9001,
            bind_address: None,
            tls_cert: None,
            tls_key: None,
            path: None,
            compression: None,
            internal: None,
        },
        GatewayListenerConfig {
            protocol: "quic".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9001,
            bind_address: None,
            tls_cert: None,
            tls_key: None,
            path: None,
            compression: None,
            internal: None,
        },
        GatewayListenerConfig {
            protocol: "websocket".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9080,
            bind_address: None,
            tls_cert: None,
            tls_key: None,
            path: None,
            compression: None,
            internal: None,
        },
    ]
}

/// 网关配置（TCP/WebSocket/QUIC）；gateway.listeners 为多监听入口
#[derive(Debug, Deserialize)]
struct TomlGatewayConfig {
    /// 多监听入口：每项 protocol + host + port
    listeners: Option<Vec<TomlListenerConfig>>,
    max_connections: Option<u32>,
    connection_timeout: Option<u64>,
    heartbeat_interval: Option<u64>,
    use_internal_auth: Option<bool>,
    handler_max_inflight: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct TomlCacheConfig {
    cache_type: Option<String>,
    l1_max_memory_mb: Option<u64>,
    l1_ttl_secs: Option<u64>,
    redis: Option<TomlRedisConfig>,
    online_status: Option<TomlOnlineStatusConfig>,
}

#[derive(Debug, Deserialize)]
struct TomlRedisConfig {
    url: Option<String>,
    pool_size: Option<u32>,
    min_idle: Option<u32>,
    connection_timeout: Option<u64>,
    command_timeout_ms: Option<u64>,
    idle_timeout: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TomlOnlineStatusConfig {
    timeout_seconds: Option<u64>,
    cleanup_interval_seconds: Option<u64>,
}

/// 单个存储源（storage_source_id）配置：无 region 字段，按 default_storage_source_id 选择
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileStorageSourceConfig {
    /// 存储源 ID，与数据库 privchat_file_uploads.storage_source_id 对应（0=本地，1/2=其他数据中心等）
    pub id: u32,
    /// 存储类型：local / s3（s3 兼容 Garage/MinIO/AWS/阿里云 OSS/腾讯云 COS 等）
    #[serde(default = "default_storage_type")]
    pub storage_type: String,
    /// 本地存储根目录（storage_type=local 时必填）
    #[serde(default)]
    pub storage_root: String,
    /// 该存储源的文件访问基础 URL（用于生成 file_url；local 与 s3 均需配置）
    pub base_url: Option<String>,
    // ---------- S3 兼容存储（storage_type=s3 时必填）----------
    /// 节点/Endpoint，如 oss-cn-hongkong.aliyuncs.com（不含协议，代码中会补 https://）
    pub endpoint: Option<String>,
    /// 桶名，如 privchat
    pub bucket: Option<String>,
    /// AccessKey（敏感，建议用环境变量覆盖）
    pub access_key_id: Option<String>,
    /// AccessSecret（敏感，建议用环境变量覆盖）
    pub secret_access_key: Option<String>,
    /// 桶内存储目录前缀，不填或空则使用桶根目录；填则 object key = path_prefix/file_path
    #[serde(default)]
    pub path_prefix: Option<String>,
}

fn default_storage_type() -> String {
    "local".to_string()
}

#[derive(Debug, Deserialize)]
struct TomlFileConfig {
    /// 存储源列表，必须至少配置一个 [[file.storage_sources]]
    storage_sources: Option<Vec<TomlFileStorageSource>>,
    default_storage_source_id: Option<u32>,
    /// HTTP 文件服务监听端口（原 file_server.port）
    server_port: Option<u16>,
    /// 文件服务 API 基础 URL，客户端访问（原 file_server.api_base_url）
    server_api_base_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlFileStorageSource {
    id: u32,
    #[serde(default = "default_storage_type")]
    storage_type: String,
    #[serde(default)]
    storage_root: String,
    base_url: Option<String>,
    // S3 兼容
    endpoint: Option<String>,
    bucket: Option<String>,
    access_key_id: Option<String>,
    secret_access_key: Option<String>,
    #[serde(default)]
    path_prefix: Option<String>,
}

/// TOML [logging] 段，用于反序列化
#[derive(Debug, Deserialize)]
struct TomlLoggingConfig {
    level: Option<String>,
    format: Option<String>,
    file: Option<String>,
}

/// 早期日志配置（在完整 ServerConfig 加载之前，快速读取 [logging] 段）
#[derive(Debug, Default)]
pub struct EarlyLoggingConfig {
    pub level: Option<String>,
    pub format: Option<String>,
    pub file: Option<String>,
}

/// 仅用于快速反序列化 config.toml 中的 [logging] 段
#[derive(Debug, Deserialize)]
struct TomlLoggingOnly {
    logging: Option<TomlLoggingConfig>,
}

/// 从配置文件快速读取 [logging] 段（不加载完整配置）
///
/// 用于在 ServerConfig::load() 之前初始化日志系统，
/// 使日志文件路径可以在 config.toml 中配置。
pub fn load_early_logging_config(config_file: Option<&str>) -> EarlyLoggingConfig {
    let path = config_file.unwrap_or("config.toml");
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return EarlyLoggingConfig::default(),
    };
    let parsed: TomlLoggingOnly = match toml::from_str(&content) {
        Ok(c) => c,
        Err(_) => return EarlyLoggingConfig::default(),
    };
    match parsed.logging {
        Some(log) => EarlyLoggingConfig {
            level: log.level,
            format: log.format,
            file: log.file,
        },
        None => EarlyLoggingConfig::default(),
    }
}

impl From<TomlConfig> for ServerConfig {
    fn from(toml: TomlConfig) -> Self {
        let mut config = Self::default();

        // 网关：gateway.listeners
        if let Some(gw) = toml.gateway {
            if let Some(max_conn) = gw.max_connections {
                config.max_connections = max_conn;
            }
            if let Some(timeout) = gw.connection_timeout {
                config.connection_timeout = timeout;
            }
            if let Some(interval) = gw.heartbeat_interval {
                config.heartbeat_interval = interval;
            }
            if let Some(use_internal) = gw.use_internal_auth {
                config.use_internal_auth = use_internal;
            }
            if let Some(max_inflight) = gw.handler_max_inflight {
                config.handler_max_inflight = max_inflight;
            }
            if let Some(ref list) = gw.listeners {
                if !list.is_empty() {
                    config.gateway_listeners = list
                        .iter()
                        .map(|l| GatewayListenerConfig {
                            protocol: l.protocol.to_lowercase(),
                            host: l.host.clone(),
                            port: l.port,
                            bind_address: None,
                            tls_cert: l.tls_cert.clone(),
                            tls_key: l.tls_key.clone(),
                            path: l.path.clone(),
                            compression: l.compression,
                            internal: l.internal,
                        })
                        .collect();
                    let mut set_tcp = false;
                    let mut set_ws = false;
                    let mut set_quic = false;
                    for l in &config.gateway_listeners {
                        match l.protocol.as_str() {
                            "tcp" if !set_tcp => {
                                config.tcp_bind_address = l.bind_address();
                                config.host = l.host.clone();
                                config.port = l.port;
                                set_tcp = true;
                            }
                            "websocket" if !set_ws => {
                                config.websocket_bind_address = l.bind_address();
                                set_ws = true;
                            }
                            "quic" if !set_quic => {
                                config.quic_bind_address = l.bind_address();
                                set_quic = true;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        if let Some(cache) = toml.cache {
            if let Some(memory_mb) = cache.l1_max_memory_mb {
                config.cache.l1_max_memory_mb = memory_mb;
            }
            if let Some(ttl) = cache.l1_ttl_secs {
                config.cache.l1_ttl_secs = ttl;
            }
            if let Some(redis) = cache.redis {
                if let Some(url) = redis.url {
                    config.cache.redis = Some(RedisConfig {
                        url,
                        pool_size: redis.pool_size.unwrap_or(50),
                        min_idle: redis.min_idle.unwrap_or(10),
                        connection_timeout_secs: redis.connection_timeout.unwrap_or(5),
                        command_timeout_ms: redis.command_timeout_ms.unwrap_or(5000),
                        idle_timeout_secs: redis.idle_timeout.unwrap_or(300),
                    });
                }
            }
            if let Some(online_status) = cache.online_status {
                if let Some(timeout) = online_status.timeout_seconds {
                    config.cache.online_status.offline_timeout_secs = timeout;
                }
                if let Some(interval) = online_status.cleanup_interval_seconds {
                    config.cache.online_status.cleanup_interval_secs = interval;
                }
            }
        }

        if let Some(file) = toml.file {
            if let Some(port) = file.server_port {
                config.http_file_server_port = port;
            }
            if let Some(api_base_url) = file.server_api_base_url {
                config.file_api_base_url = Some(api_base_url);
            }
            if let Some(sources) = file.storage_sources {
                config.file_storage_sources = sources
                    .into_iter()
                    .map(|s| FileStorageSourceConfig {
                        id: s.id,
                        storage_type: s.storage_type,
                        storage_root: s.storage_root,
                        base_url: s.base_url,
                        endpoint: s.endpoint,
                        bucket: s.bucket,
                        access_key_id: s.access_key_id,
                        secret_access_key: s.secret_access_key,
                        path_prefix: s.path_prefix,
                    })
                    .collect();
            }
            if let Some(id) = file.default_storage_source_id {
                config.file_default_storage_source_id = id;
            }
        }

        if let Some(admin) = toml.admin {
            if let Some(port) = admin.port {
                config.admin_api_port = port;
            }
            if let Some(key) = admin.master_key {
                config.service_master_key = key;
            }
        }

        if let Some(system_msg) = toml.system_message {
            if let Some(enabled) = system_msg.enabled {
                config.system_message.enabled = enabled;
            }
            if let Some(welcome_msg) = system_msg.welcome_message {
                config.system_message.welcome_message = welcome_msg;
            }
            if let Some(auto_create) = system_msg.auto_create_channel {
                config.system_message.auto_create_channel = auto_create;
            }
            if let Some(auto_send) = system_msg.auto_send_welcome {
                config.system_message.auto_send_welcome = auto_send;
            }
        }

        if let Some(push) = toml.push {
            if let Some(enabled) = push.enabled {
                config.push.enabled = enabled;
            }
            if let Some(apns) = push.apns {
                if let Some(enabled) = apns.enabled {
                    config.push.apns.enabled = enabled;
                }
                if let Some(bundle_id) = apns.bundle_id {
                    config.push.apns.bundle_id = Some(bundle_id);
                }
                if let Some(team_id) = apns.team_id {
                    config.push.apns.team_id = Some(team_id);
                }
                if let Some(key_id) = apns.key_id {
                    config.push.apns.key_id = Some(key_id);
                }
                if let Some(private_key_path) = apns.private_key_path {
                    config.push.apns.private_key_path = Some(private_key_path);
                }
                if let Some(use_sandbox) = apns.use_sandbox {
                    config.push.apns.use_sandbox = use_sandbox;
                }
            }
            if let Some(fcm) = push.fcm {
                if let Some(enabled) = fcm.enabled {
                    config.push.fcm.enabled = enabled;
                }
                if let Some(project_id) = fcm.project_id {
                    config.push.fcm.project_id = Some(project_id);
                }
                if let Some(access_token) = fcm.access_token {
                    config.push.fcm.access_token = Some(access_token);
                }
            }
            if let Some(hms) = push.hms {
                if let Some(enabled) = hms.enabled {
                    config.push.hms.enabled = enabled;
                }
                if let Some(app_id) = hms.app_id {
                    config.push.hms.app_id = Some(app_id);
                }
                if let Some(access_token) = hms.access_token {
                    config.push.hms.access_token = Some(access_token);
                }
                if let Some(endpoint) = hms.endpoint {
                    config.push.hms.endpoint = Some(endpoint);
                }
            }
            if let Some(honor) = push.honor {
                if let Some(enabled) = honor.enabled {
                    config.push.honor.enabled = enabled;
                }
                if let Some(app_id) = honor.app_id {
                    config.push.honor.app_id = Some(app_id);
                }
                if let Some(access_token) = honor.access_token {
                    config.push.honor.access_token = Some(access_token);
                }
                if let Some(endpoint) = honor.endpoint {
                    config.push.honor.endpoint = Some(endpoint);
                }
            }
            if let Some(xiaomi) = push.xiaomi {
                if let Some(enabled) = xiaomi.enabled {
                    config.push.xiaomi.enabled = enabled;
                }
                if let Some(app_id) = xiaomi.app_id {
                    config.push.xiaomi.app_id = Some(app_id);
                }
                if let Some(access_token) = xiaomi.access_token {
                    config.push.xiaomi.access_token = Some(access_token);
                }
                if let Some(endpoint) = xiaomi.endpoint {
                    config.push.xiaomi.endpoint = Some(endpoint);
                }
            }
            if let Some(oppo) = push.oppo {
                if let Some(enabled) = oppo.enabled {
                    config.push.oppo.enabled = enabled;
                }
                if let Some(app_id) = oppo.app_id {
                    config.push.oppo.app_id = Some(app_id);
                }
                if let Some(access_token) = oppo.access_token {
                    config.push.oppo.access_token = Some(access_token);
                }
                if let Some(endpoint) = oppo.endpoint {
                    config.push.oppo.endpoint = Some(endpoint);
                }
            }
            if let Some(vivo) = push.vivo {
                if let Some(enabled) = vivo.enabled {
                    config.push.vivo.enabled = enabled;
                }
                if let Some(app_id) = vivo.app_id {
                    config.push.vivo.app_id = Some(app_id);
                }
                if let Some(access_token) = vivo.access_token {
                    config.push.vivo.access_token = Some(access_token);
                }
                if let Some(endpoint) = vivo.endpoint {
                    config.push.vivo.endpoint = Some(endpoint);
                }
            }
            if let Some(lenovo) = push.lenovo {
                if let Some(enabled) = lenovo.enabled {
                    config.push.lenovo.enabled = enabled;
                }
                if let Some(app_id) = lenovo.app_id {
                    config.push.lenovo.app_id = Some(app_id);
                }
                if let Some(access_token) = lenovo.access_token {
                    config.push.lenovo.access_token = Some(access_token);
                }
                if let Some(endpoint) = lenovo.endpoint {
                    config.push.lenovo.endpoint = Some(endpoint);
                }
            }
            if let Some(zte) = push.zte {
                if let Some(enabled) = zte.enabled {
                    config.push.zte.enabled = enabled;
                }
                if let Some(app_id) = zte.app_id {
                    config.push.zte.app_id = Some(app_id);
                }
                if let Some(access_token) = zte.access_token {
                    config.push.zte.access_token = Some(access_token);
                }
                if let Some(endpoint) = zte.endpoint {
                    config.push.zte.endpoint = Some(endpoint);
                }
            }
            if let Some(meizu) = push.meizu {
                if let Some(enabled) = meizu.enabled {
                    config.push.meizu.enabled = enabled;
                }
                if let Some(app_id) = meizu.app_id {
                    config.push.meizu.app_id = Some(app_id);
                }
                if let Some(access_token) = meizu.access_token {
                    config.push.meizu.access_token = Some(access_token);
                }
                if let Some(endpoint) = meizu.endpoint {
                    config.push.meizu.endpoint = Some(endpoint);
                }
            }
        }

        config
    }
}

/// 缓存配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    /// L1缓存最大内存（MB）
    pub l1_max_memory_mb: u64,
    /// L1缓存TTL（秒）
    pub l1_ttl_secs: u64,
    /// Redis配置（可选）
    pub redis: Option<RedisConfig>,
    /// 在线状态配置
    pub online_status: OnlineStatusConfig,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            l1_max_memory_mb: 256,
            l1_ttl_secs: 3600, // 1 hour TTL
            redis: None,
            online_status: OnlineStatusConfig::default(),
        }
    }
}

impl CacheConfig {
    /// 获取L1缓存TTL
    pub fn l1_ttl(&self) -> Duration {
        Duration::from_secs(self.l1_ttl_secs)
    }

    /// 检查是否有Redis配置
    pub fn has_redis(&self) -> bool {
        self.redis.is_some()
    }
}

/// Redis配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RedisConfig {
    /// Redis连接URL
    pub url: String,
    /// 连接池最大连接数
    pub pool_size: u32,
    /// 连接池最小空闲连接数
    pub min_idle: u32,
    /// 连接超时时间（秒）— 从池获取连接的超时
    pub connection_timeout_secs: u64,
    /// 命令执行超时时间（毫秒）— 单条 Redis 命令的超时
    pub command_timeout_ms: u64,
    /// 空闲连接超时时间（秒）— 超过此时间的空闲连接被回收
    pub idle_timeout_secs: u64,
}

impl RedisConfig {
    /// 获取连接超时时间
    pub fn connection_timeout(&self) -> Duration {
        Duration::from_secs(self.connection_timeout_secs)
    }

    /// 获取命令执行超时时间
    pub fn command_timeout(&self) -> Duration {
        Duration::from_millis(self.command_timeout_ms)
    }

    /// 获取空闲连接超时时间
    pub fn idle_timeout(&self) -> Duration {
        Duration::from_secs(self.idle_timeout_secs)
    }
}

/// 推送总配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushConfig {
    /// 推送总开关
    pub enabled: bool,
    /// APNs 配置（iOS）
    pub apns: PushApnsConfig,
    /// FCM 配置（Android）
    pub fcm: PushFcmConfig,
    /// HMS 配置（Huawei / HarmonyOS / Honor）
    pub hms: PushHmsConfig,
    /// Honor 配置（协议复用 HMS，但凭证独立）
    pub honor: PushHonorConfig,
    /// Xiaomi 配置
    pub xiaomi: PushXiaomiConfig,
    /// OPPO 配置
    pub oppo: PushOppoConfig,
    /// Vivo 配置
    pub vivo: PushVivoConfig,
    /// Lenovo 配置
    pub lenovo: PushLenovoConfig,
    /// ZTE 配置
    pub zte: PushZteConfig,
    /// Meizu 配置
    pub meizu: PushMeizuConfig,
}

impl Default for PushConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            apns: PushApnsConfig::default(),
            fcm: PushFcmConfig::default(),
            hms: PushHmsConfig::default(),
            honor: PushHonorConfig::default(),
            xiaomi: PushXiaomiConfig::default(),
            oppo: PushOppoConfig::default(),
            vivo: PushVivoConfig::default(),
            lenovo: PushLenovoConfig::default(),
            zte: PushZteConfig::default(),
            meizu: PushMeizuConfig::default(),
        }
    }
}

/// APNs 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushApnsConfig {
    pub enabled: bool,
    pub bundle_id: Option<String>,
    pub team_id: Option<String>,
    pub key_id: Option<String>,
    pub private_key_path: Option<String>,
    pub use_sandbox: bool,
}

impl Default for PushApnsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bundle_id: None,
            team_id: None,
            key_id: None,
            private_key_path: None,
            use_sandbox: false,
        }
    }
}

/// FCM 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushFcmConfig {
    pub enabled: bool,
    pub project_id: Option<String>,
    pub access_token: Option<String>,
}

impl Default for PushFcmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            project_id: None,
            access_token: None,
        }
    }
}

/// HMS 配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushHmsConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    /// 可选 API 地址，默认 `https://push-api.cloud.huawei.com`
    pub endpoint: Option<String>,
}

impl Default for PushHmsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushHonorConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushHonorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushXiaomiConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushXiaomiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushOppoConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushOppoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushVivoConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushVivoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushLenovoConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushLenovoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushZteConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushZteConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushMeizuConfig {
    pub enabled: bool,
    pub app_id: Option<String>,
    pub access_token: Option<String>,
    pub endpoint: Option<String>,
}

impl Default for PushMeizuConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: None,
            access_token: None,
            endpoint: None,
        }
    }
}

/// 在线状态管理器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnlineStatusConfig {
    /// 离线超时时间（秒）
    pub offline_timeout_secs: u64,
    /// 清理间隔（秒）
    pub cleanup_interval_secs: u64,
}

impl Default for OnlineStatusConfig {
    fn default() -> Self {
        Self {
            offline_timeout_secs: 300,
            cleanup_interval_secs: 60,
        }
    }
}

// =====================================================
// 系统用户管理
// =====================================================
//
// 系统用户定义在服务启动时加载到内存中
// 不存在于数据库，通过预定义数组管理
//
// 用户 ID 区间划分：
// - 1 ~ 99: 保留给系统功能用户
// - 100,000,000+: 普通用户 + 机器人（用 user_type 区分）
// =====================================================

use std::collections::HashSet;
use std::sync::OnceLock;

/// 系统用户定义（与普通用户使用相同结构，仅 user_type = 1）
#[derive(Debug, Clone)]
pub struct SystemUserDef {
    pub user_id: u64,
    pub username: String,
    pub display_name: String, // 英文默认名（客户端根据语言包替换）
    pub description: String,
}

/// 全局系统用户列表（服务启动时初始化）
static SYSTEM_USERS: OnceLock<Vec<SystemUserDef>> = OnceLock::new();
static SYSTEM_USER_IDS: OnceLock<HashSet<u64>> = OnceLock::new();

/// 系统消息用户 ID
pub const SYSTEM_USER_ID: u64 = 1;

/// 普通用户 ID 起始值（数据库序列从此值开始）
pub const NORMAL_USER_ID_START: u64 = 100_000_000;

/// 初始化系统用户列表（服务启动时调用一次）
pub fn init_system_users() {
    let users = vec![
        SystemUserDef {
            user_id: SYSTEM_USER_ID,
            username: String::new(),
            display_name: "System Message".to_string(),
            description: "System notifications".to_string(),
        },
        // 未来扩展：
        // SystemUserDef {
        //     user_id: FILE_HELPER_ID,
        //     username: String::new(),
        //     display_name: "File Transfer".to_string(),
        //     description: "自己和自己的文件传输".to_string(),
        // },
    ];

    // 构建 ID 集合用于快速查询
    let ids: HashSet<u64> = users.iter().map(|u| u.user_id).collect();

    let _ = SYSTEM_USERS.set(users);
    let _ = SYSTEM_USER_IDS.set(ids);
}

/// 判断是否为系统用户
pub fn is_system_user(user_id: u64) -> bool {
    SYSTEM_USER_IDS
        .get()
        .map(|ids| ids.contains(&user_id))
        .unwrap_or(false)
}

/// 获取系统用户定义
pub fn get_system_user(user_id: u64) -> Option<&'static SystemUserDef> {
    SYSTEM_USERS
        .get()
        .and_then(|users| users.iter().find(|u| u.user_id == user_id))
}

/// 获取所有系统用户
pub fn get_all_system_users() -> Option<&'static Vec<SystemUserDef>> {
    SYSTEM_USERS.get()
}

/// 系统消息配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemMessageConfig {
    /// 是否启用系统消息用户
    pub enabled: bool,
    /// 欢迎消息内容
    pub welcome_message: String,
    /// 是否在用户注册时自动创建会话
    pub auto_create_channel: bool,
    /// 是否在创建会话后自动发送欢迎消息
    pub auto_send_welcome: bool,
}

impl Default for SystemMessageConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            welcome_message: "👋 欢迎使用 Privchat！\n\n这是一个端到端加密的即时通讯系统。"
                .to_string(),
            auto_create_channel: true,
            auto_send_welcome: true,
        }
    }
}

#[derive(Debug, Deserialize)]
struct TomlSystemMessageConfig {
    enabled: Option<bool>,
    welcome_message: Option<String>,
    auto_create_channel: Option<bool>,
    auto_send_welcome: Option<bool>,
}

// =====================================================
// 安全防护配置
// =====================================================

/// 安全防护配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityProtectionConfig {
    /// 安全模式
    /// - "observe": 只记录，不处罚（早期推荐）
    /// - "enforce_light": 轻量限流
    /// - "enforce_full": 全部特性
    #[serde(default = "default_security_mode")]
    pub mode: String,

    /// 是否启用 Shadow Ban
    pub enable_shadow_ban: bool,

    /// 是否启用 IP 封禁
    pub enable_ip_ban: bool,

    /// 速率限制配置
    pub rate_limit: RateLimitProtectionConfig,
}

fn default_security_mode() -> String {
    "observe".to_string()
}

impl Default for SecurityProtectionConfig {
    fn default() -> Self {
        Self {
            mode: "observe".to_string(), // 默认观察模式
            enable_shadow_ban: false,    // 默认不启用
            enable_ip_ban: true,
            rate_limit: RateLimitProtectionConfig::default(),
        }
    }
}

/// 速率限制配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitProtectionConfig {
    /// 用户全局：每秒令牌数
    pub user_tokens_per_second: f64,
    /// 用户全局：桶容量（允许突发）
    pub user_burst_capacity: f64,

    /// 单会话：每秒消息数
    pub channel_messages_per_second: f64,
    /// 单会话：桶容量
    pub channel_burst_capacity: f64,

    /// IP 连接：每秒连接数
    pub ip_connections_per_second: f64,
    /// IP 连接：桶容量
    pub ip_burst_capacity: f64,
}

impl Default for RateLimitProtectionConfig {
    fn default() -> Self {
        Self {
            // 用户全局：基础 50 tokens/s，突发 100
            user_tokens_per_second: 50.0,
            user_burst_capacity: 100.0,

            // 单会话：3条消息/秒（考虑到大群的 fan-out）
            channel_messages_per_second: 3.0,
            channel_burst_capacity: 10.0,

            // IP 连接：5个/秒
            ip_connections_per_second: 5.0,
            ip_burst_capacity: 10.0,
        }
    }
}

impl From<RateLimitProtectionConfig> for crate::security::RateLimitConfig {
    fn from(config: RateLimitProtectionConfig) -> Self {
        crate::security::RateLimitConfig {
            user_tokens_per_second: config.user_tokens_per_second,
            user_burst_capacity: config.user_burst_capacity,
            channel_messages_per_second: config.channel_messages_per_second,
            channel_burst_capacity: config.channel_burst_capacity,
            ip_connections_per_second: config.ip_connections_per_second,
            ip_burst_capacity: config.ip_burst_capacity,
        }
    }
}

impl From<SecurityProtectionConfig> for crate::security::SecurityConfig {
    fn from(config: SecurityProtectionConfig) -> Self {
        use crate::security::SecurityMode;

        let mode = match config.mode.as_str() {
            "observe" | "observe_only" => SecurityMode::ObserveOnly,
            "enforce_light" | "light" => SecurityMode::EnforceLight,
            "enforce_full" | "full" => SecurityMode::EnforceFull,
            _ => {
                tracing::warn!("未知的安全模式: {}，使用默认 ObserveOnly", config.mode);
                SecurityMode::ObserveOnly
            }
        };

        crate::security::SecurityConfig {
            mode,
            enable_shadow_ban: config.enable_shadow_ban,
            enable_ip_ban: config.enable_ip_ban,
            rate_limit: config.rate_limit.into(),
        }
    }
}
