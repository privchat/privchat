use std::time::Duration;
use std::env;
use std::fs;
use std::path::Path;
use tracing::info;
use serde::{Deserialize, Serialize};
use anyhow::{Result, Context};

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
    /// TCP 监听地址
    pub tcp_bind_address: String,
    /// WebSocket 监听地址
    pub websocket_bind_address: String,
    /// QUIC 监听地址
    pub quic_bind_address: String,
    /// 文件存储根目录（兼容旧配置；若配置了 file.storage_sources 则以此为准）
    pub file_storage_root: String,
    /// 文件基础 URL（兼容旧配置；若配置了 file.storage_sources 则以此为准）
    pub file_base_url: Option<String>,
    /// 存储源列表（必须至少配置一个 [[file.storage_sources]]；未配置时由 storage_root/base_url 构造 id=0 默认源）
    pub file_storage_sources: Vec<FileStorageSourceConfig>,
    /// 默认存储源 ID（上传时使用，须在 file_storage_sources 中存在）
    pub file_default_storage_source_id: u32,
    /// HTTP 文件服务器端口（用于启动服务）
    pub http_file_server_port: u16,
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 8080,
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/privchat".to_string()),
            jwt_secret: "your_jwt_secret_here".to_string(),
            max_connections: 1000,
            connection_timeout: 300,
            heartbeat_interval: 60,
            log_level: "info".to_string(),
            enable_tls: false,
            tls_cert_path: None,
            tls_key_path: None,
            cache: CacheConfig::default(),
            enabled_protocols: vec!["tcp".to_string(), "websocket".to_string(), "quic".to_string()],
            tcp_bind_address: "0.0.0.0:8080".to_string(),
            websocket_bind_address: "0.0.0.0:8081".to_string(),
            quic_bind_address: "0.0.0.0:8082".to_string(),
            file_storage_root: "./storage/files".to_string(),
            file_base_url: Some("http://localhost:8083".to_string()),
            file_storage_sources: vec![],
            file_default_storage_source_id: 0,
            http_file_server_port: 8083,
            file_api_base_url: Some("http://localhost:8083/api/app".to_string()),
            use_internal_auth: true, // 默认启用内置账号系统（方便独立部署和测试）
            system_message: SystemMessageConfig::default(),
            security: SecurityProtectionConfig::default(),
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
            pool_size: 10,
            connection_timeout_secs: 5,
        });
        self
    }

    /// 从 TOML 文件加载配置
    pub fn from_toml_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = fs::read_to_string(path.as_ref())
            .with_context(|| format!("无法读取配置文件: {:?}", path.as_ref()))?;
        
        let toml_config: TomlConfig = toml::from_str(&content)
            .with_context(|| "配置文件格式错误")?;
        
        Ok(toml_config.into())
    }

    /// 从环境变量加载配置（PRIVCHAT_ 前缀）
    pub fn merge_from_env(&mut self) -> Result<()> {
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
        if let Ok(log_level) = env::var("PRIVCHAT_LOG_LEVEL") {
            self.log_level = log_level;
        }
        if let Ok(_log_format) = env::var("PRIVCHAT_LOG_FORMAT") {
            // 将在日志初始化时使用
        }
        
        // Redis 配置
        if let Ok(redis_url) = env::var("REDIS_URL") {
            self.cache.redis = Some(RedisConfig {
                url: redis_url,
                pool_size: 10,
                connection_timeout_secs: 5,
            });
        }
        
        // 文件配置
        if let Ok(storage_root) = env::var("PRIVCHAT_FILE_STORAGE_ROOT") {
            self.file_storage_root = storage_root;
        }
        if let Ok(base_url) = env::var("PRIVCHAT_FILE_BASE_URL") {
            self.file_base_url = Some(base_url);
        }
        if let Ok(file_api_url) = env::var("PRIVCHAT_FILE_API_BASE_URL") {
            self.file_api_base_url = Some(file_api_url);
        }
        
        Ok(())
    }

    /// 获取有效的文件存储源列表。必须至少配置一个：若配置了 storage_sources 则用其，否则用单一 storage_root/base_url 构造 id=0 的默认源（兼容旧配置）
    pub fn effective_file_storage_sources(&self) -> Vec<FileStorageSourceConfig> {
        if self.file_storage_sources.is_empty() {
            vec![FileStorageSourceConfig {
                id: 0,
                storage_type: "local".to_string(),
                storage_root: self.file_storage_root.clone(),
                base_url: self.file_base_url.clone(),
                endpoint: None,
                bucket: None,
                access_key_id: None,
                secret_access_key: None,
                path_prefix: None,
            }]
        } else {
            self.file_storage_sources.clone()
        }
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
            self.cache.redis = Some(RedisConfig {
                url: redis_url.clone(),
                pool_size: 10,
                connection_timeout_secs: 5,
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
                _ => Self::default()
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

        Ok(config)
    }
}

/// TOML 配置文件结构（用于反序列化）
#[derive(Debug, Deserialize)]
struct TomlConfig {
    server: Option<TomlServerConfig>,
    cache: Option<TomlCacheConfig>,
    file: Option<TomlFileConfig>,
    logging: Option<TomlLoggingConfig>,
    system_message: Option<TomlSystemMessageConfig>,
}

#[derive(Debug, Deserialize)]
struct TomlServerConfig {
    host: Option<String>,
    port: Option<u16>,
    http_file_server_port: Option<u16>,
    file_api_base_url: Option<String>,  // 文件服务 API 基础 URL
    max_connections: Option<u32>,
    connection_timeout: Option<u64>,
    heartbeat_interval: Option<u64>,
    use_internal_auth: Option<bool>,
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
    connection_timeout: Option<u64>,
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
    storage_root: Option<String>,
    base_url: Option<String>,
    /// 存储源列表，必须至少配置一个 [[file.storage_sources]]
    storage_sources: Option<Vec<TomlFileStorageSource>>,
    default_storage_source_id: Option<u32>,
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

#[derive(Debug, Deserialize)]
struct TomlLoggingConfig {
    level: Option<String>,
    format: Option<String>,
    file: Option<String>,
}

impl From<TomlConfig> for ServerConfig {
    fn from(toml: TomlConfig) -> Self {
        let mut config = Self::default();
        
        if let Some(server) = toml.server {
            if let Some(host) = server.host {
                config.host = host;
            }
            if let Some(port) = server.port {
                config.port = port;
                config.tcp_bind_address = format!("{}:{}", config.host, port);
            }
            if let Some(http_port) = server.http_file_server_port {
                config.http_file_server_port = http_port;
            }
            if let Some(file_api_url) = server.file_api_base_url {
                config.file_api_base_url = Some(file_api_url);
            }
            if let Some(max_conn) = server.max_connections {
                config.max_connections = max_conn;
            }
            if let Some(timeout) = server.connection_timeout {
                config.connection_timeout = timeout;
            }
            if let Some(interval) = server.heartbeat_interval {
                config.heartbeat_interval = interval;
            }
            if let Some(use_internal) = server.use_internal_auth {
                config.use_internal_auth = use_internal;
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
                        pool_size: redis.pool_size.unwrap_or(10),
                        connection_timeout_secs: redis.connection_timeout.unwrap_or(5),
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
            if let Some(storage_root) = file.storage_root {
                config.file_storage_root = storage_root;
            }
            if let Some(base_url) = file.base_url {
                config.file_base_url = Some(base_url);
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
    /// 连接池大小
    pub pool_size: u32,
    /// 连接超时时间（秒）
    pub connection_timeout_secs: u64,
}

impl RedisConfig {
    /// 获取连接超时时间
    pub fn connection_timeout(&self) -> Duration {
        Duration::from_secs(self.connection_timeout_secs)
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

use std::sync::OnceLock;
use std::collections::HashSet;

/// 系统用户定义（与普通用户使用相同结构，仅 user_type = 1）
#[derive(Debug, Clone)]
pub struct SystemUserDef {
    pub user_id: u64,
    pub username: String,
    pub display_name: String,  // 英文默认名（客户端根据语言包替换）
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
            welcome_message: "👋 欢迎使用 Privchat！\n\n这是一个端到端加密的即时通讯系统。".to_string(),
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
            mode: "observe".to_string(),  // 默认观察模式
            enable_shadow_ban: false,     // 默认不启用
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