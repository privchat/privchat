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

/// 附件加密密钥表。`Debug` 只报数量与 id，绝不渲染密钥本身。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct AttachmentKeys(pub Vec<(u8, String)>);

impl std::fmt::Debug for AttachmentKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AttachmentKeys({} keys, ids={:?}, material=[REDACTED])",
            self.0.len(),
            self.0.iter().map(|(id, _)| *id).collect::<Vec<_>>()
        )
    }
}

/// 秒传索引用的长期密钥。`Debug` 只报「有没有配」，绝不渲染密钥本身。
///
/// 🔴 **必须独立于 [`AttachmentKeys`]，不得由它派生。** 加密密钥是可轮换的；
/// 一旦 dedup 密钥跟着轮换，同一份明文换算出的 `dedup_id` 就变了，**全部历史对象的
/// 跨代秒传直接失效**。它需要独立备份与灾难恢复，真要轮换只能走双索引迁移。
///
/// 🔴 **绝不下发给客户端。** 客户端算不出 `dedup_id`，服务端才有能力在首传校验时
/// 独立判定「你声明的身份和你传的字节是不是同一份内容」。
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct DedupMasterKey(pub Option<Vec<u8>>);

impl std::fmt::Debug for DedupMasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Some(_) => write!(f, "DedupMasterKey(configured, material=[REDACTED])"),
            None => write!(f, "DedupMasterKey(absent)"),
        }
    }
}

impl DedupMasterKey {
    /// `dedup_id = HMAC-SHA256(dedup_master_key, "privchat-attachment-dedup-v1" || plaintext_sha256)`
    ///
    /// 输入是明文摘要的**原始字节**，不是它的十六进制串——换成十六进制并不会更安全，
    /// 但两端只要有一处写法不同，全站秒传就静默失效且没有任何报错。
    pub fn dedup_id(&self, plaintext_sha256: &[u8; 32]) -> Option<String> {
        use hmac::{Hmac, Mac};
        let key = self.0.as_ref()?;
        let mut mac = <Hmac<sha2::Sha256> as Mac>::new_from_slice(key)
            .expect("HMAC accepts keys of any length");
        mac.update(DEDUP_INFO);
        mac.update(plaintext_sha256);
        Some(hex::encode(mac.finalize().into_bytes()))
    }

    pub fn is_configured(&self) -> bool {
        self.0.is_some()
    }
}

const DEDUP_INFO: &[u8] = b"privchat-attachment-dedup-v1";

impl AttachmentKeys {
    pub fn first(&self) -> Option<&(u8, String)> {
        self.0.first()
    }
    pub fn iter(&self) -> std::slice::Iter<'_, (u8, String)> {
        self.0.iter()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// 服务器配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// 服务器监听地址
    pub host: String,
    /// 服务器监听端口
    pub port: u16,
    /// 数据库连接字符串
    pub database_url: String,
    /// 数据库连接池与查询超时配置
    pub database: DatabaseConfig,
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
    /// Room 订阅回放配置
    pub room: RoomConfig,
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
    /// 附件加密密钥（v2）。第一项是当前使用的，其余是保留下来给老对象解密用的。
    ///
    /// 🔴 密钥只经由**已鉴权**的接口下发给客户端，明文与密钥都不进对象存储——
    /// 威胁模型是存储服务商本身（ATTACHMENT_ENCRYPTION_SPEC §0.1）。
    /// 空 = 未启用 v2，客户端沿用 v1 的 per-file 随机 CEK。
    ///
    /// 🔴 用 [`AttachmentKeys`] 而不是裸 `Vec<(u8, String)>`：`ServerConfig` 派生了
    /// `Debug`，一次 `{:?}` 就能把全部密钥打进日志。包一层手写 Debug 的类型，
    /// 让脱敏成为类型自带的性质，而不是「记得别打印」。
    #[serde(skip_serializing)]
    pub attachment_keys: AttachmentKeys,
    /// 跨用户秒传索引的长期密钥（见 [`DedupMasterKey`]）。空 = 不做跨用户秒传。
    #[serde(skip_serializing)]
    pub dedup_master_key: DedupMasterKey,
    /// 文件 HTTP 服务的监听地址。TLS 由 nginx 终结时必须是 127.0.0.1，
    /// 否则后端端口对外可达、绕开 nginx 就是明文上传接口。
    pub http_file_server_host: String,
    /// 管理 API 服务器端口（仅内网访问）
    pub admin_api_port: u16,
    /// 文件服务 API 基础 URL（用于客户端访问，不包含端口号）
    ///
    /// 文件服务的 HTTP 服务器是独立的，客户端通过此 URL 访问文件相关接口。
    /// 例如：https://files.example.com/api/app
    ///
    /// 注意：此 URL 不包含端口号，生产环境通常通过域名访问（80/443 端口）
    pub file_api_base_url: Option<String>,
    /// 账号体系归属（spec ACCOUNT_MODE）。
    ///
    /// - [`AccountMode::Builtin`]：使用 server 内置账号系统（注册 / 登录 / refresh 全在本进程）
    /// - [`AccountMode::Platform`]：使用 privchat platform（外部）账号系统；server 仅负责 IM 通道
    ///   认证 token 由 platform 签发，server 端用户面 RPC（`account/auth/login`、
    ///   `account/user/register`、`account/auth/refresh`）一律 forbidden。
    pub account: AccountConfig,
    /// 系统消息配置
    pub system_message: SystemMessageConfig,

    /// 消息相关配置（撤回时效等）
    #[serde(default)]
    pub message: MessageConfig,
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
    /// 统一 token 配置（HTTP API + IM RPC 共用，单一签发/验证路径）。
    pub jwt: JwtConfig,
    /// Server Event 出站配置（spec 02-server/SERVER_EVENT_DISPATCH_SPEC §3）。
    ///
    /// `None` 表示未配 `[server_event]`：server 内部 emit 的事件（bot.followed /
    /// bot.unfollowed / 等）不会推给 application；业务持久化照常完成，仅缺事件
    /// 通知（application 也不会自动写 `privchat_business_channel` binding）。
    #[serde(default)]
    pub server_event: Option<ServerEventConfig>,
    /// Room subscribe ticket 配置（spec 02-server/ROOM_CHANNEL_SPEC §4）。
    ///
    /// `None` 表示未配 `[room_ticket]`：Room 订阅退化为"已认证即放行"（v1
    /// 兼容模式，直播间能跑但没有强访问控制）。配 secret 后进入完整 ticket
    /// 校验：cid / ct / did / scope / exp 都必校验。
    #[serde(default)]
    pub room_ticket: Option<RoomTicketConfig>,

    /// `[upload.token]`。`None` 表示未配置：只能签发/验证旧的 Redis UUID token，
    /// 分片上传链路不可用（等价于关停阀常开）。
    #[serde(skip)]
    pub upload_token: Option<crate::security::upload_token::UploadTokenConfig>,
    /// QR 二维码 URL 基址（spec 02-server/QR_CODE_SPEC v1.3 §7.2）。
    ///
    /// 用于拼 `qr_code` 响应字段：`{qr_base_url}/privchat:protocol/<entity>/<action>?qrkey=...`
    ///
    /// 部署方可覆写为自有品牌域名，可带 sub-path（`https://example.com/app`），
    /// 但**不能**已经带 `/privchat:protocol` 前缀（builder 会双拼）。
    ///
    /// 启动期 normalize 由 [`crate::rpc::qr::normalize_qr_base_url`] 完成，
    /// 校验失败 server 拒启动。
    #[serde(default = "default_qr_base_url")]
    pub qr_base_url: String,

    /// 未认证连接 watchdog 超时（秒）。
    /// 详见 spec `02-server/SESSION_LIFECYCLE_SPEC.md`。
    ///
    /// transport 建立后 N 秒内必须完成认证（state 转 `Authenticated`），
    /// 否则 watchdog 主动 `force_close_session` 释放 transport + 移除 Index B。
    /// 给 client 留出 access_token refresh + 重 Authenticate 的窗口。
    ///
    /// **`0` 表示禁用** watchdog（开发调试用，断点暂停时不会被踢）。
    /// 生产默认 `90`，可调 `60` / `120`。
    #[serde(default = "default_unauth_session_timeout_secs")]
    pub unauth_session_timeout_secs: u64,

    /// 未认证连接 watchdog 扫描周期（秒）。
    /// `unauth_session_timeout_secs = 0` 时此参数无效。
    ///
    /// 默认 `30`：每 30s 扫一遍 Index B 找 `state == Connecting` 且
    /// 超 `unauth_session_timeout_secs` 的条目。
    #[serde(default = "default_unauth_cleanup_interval_secs")]
    pub unauth_cleanup_interval_secs: u64,
}

fn default_qr_base_url() -> String {
    "https://privchat.app".to_string()
}

fn default_unauth_session_timeout_secs() -> u64 {
    90
}

fn default_unauth_cleanup_interval_secs() -> u64 {
    30
}

/// JWT 算法（配置 `[auth.jwt] algorithm`）。
///
/// - [`Hs256`](JwtAlgorithm::Hs256)：对称 secret，简单部署
/// - [`Rs256`](JwtAlgorithm::Rs256)：非对称 PEM key + JWKS 暴露公钥，跨服务验签更安全
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum JwtAlgorithm {
    Hs256,
    Rs256,
}

impl Default for JwtAlgorithm {
    fn default() -> Self {
        JwtAlgorithm::Rs256
    }
}

/// 统一 token 配置（spec TOKEN_UNIFICATION_SPEC v1.3 §4 / §6）。
///
/// HTTP API（/api/service/auth/issue 等）与 IM RPC（AuthorizationRequest）使用
/// **同一** `TokenService` 实例签发与验证；算法由 [`algorithm`](JwtConfig::algorithm) 决定。
///
/// 字段语义按算法分组：
/// - [`JwtAlgorithm::Hs256`]：仅用 [`secret`](JwtConfig::secret)；其它密钥字段忽略
/// - [`JwtAlgorithm::Rs256`]：用 [`private_key_path`](JwtConfig::private_key_path)、
///   [`public_key_path`](JwtConfig::public_key_path)、[`kid`](JwtConfig::kid)；JWKS 端点暴露公钥
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwtConfig {
    /// 签名/验签算法；fail-fast 启动校验（缺密钥则报错）
    pub algorithm: JwtAlgorithm,
    /// HS256 共享密钥（仅 [`JwtAlgorithm::Hs256`] 使用）
    #[serde(default)]
    pub secret: String,
    /// RS256 私钥 PEM 文件路径（仅 [`JwtAlgorithm::Rs256`] 使用）
    #[serde(default)]
    pub private_key_path: String,
    /// RS256 公钥 PEM 文件路径（仅 [`JwtAlgorithm::Rs256`] 使用）
    #[serde(default)]
    pub public_key_path: String,
    /// 当前签名 key 的 kid（JWT header `kid` claim；HS256 也写，便于轮换审计）
    pub kid: String,
    /// access token TTL 秒；spec 锁定 1h
    pub access_ttl_secs: i64,
    /// refresh token TTL 秒；spec 锁定 7d
    pub refresh_ttl_secs: i64,
    /// 颁发方 issuer claim；锁定 "privchat-server"
    pub issuer: String,
    /// 默认 audience 列表；application + IM 都接受
    pub default_audience: Vec<String>,
}

impl Default for JwtConfig {
    fn default() -> Self {
        Self {
            algorithm: JwtAlgorithm::default(),
            secret: String::new(),
            private_key_path: String::new(),
            public_key_path: String::new(),
            kid: "v1".to_string(),
            access_ttl_secs: 3600,
            // 30 天（之前 7 天偏短）。access 1h 不变，靠静默 refresh；30 天让长期活跃用户不掉线。
            refresh_ttl_secs: 2592000,
            issuer: "privchat-server".to_string(),
            default_audience: vec![
                "privchat-application".to_string(),
                "privchat-server".to_string(),
            ],
        }
    }
}

impl JwtConfig {
    /// 启动期校验：算法对应的密钥必须配齐，否则 fail-fast。
    pub fn validate(&self) -> Result<(), String> {
        match self.algorithm {
            JwtAlgorithm::Hs256 => {
                if self.secret.trim().is_empty() {
                    return Err(
                        "[auth.jwt] algorithm=HS256 但 secret 未配置（NETON_JWT_SECRET 可覆盖）"
                            .to_string(),
                    );
                }
            }
            JwtAlgorithm::Rs256 => {
                if self.private_key_path.trim().is_empty() || self.public_key_path.trim().is_empty()
                {
                    return Err(
                        "[auth.jwt] algorithm=RS256 但 private_key_path 或 public_key_path 未配置"
                            .to_string(),
                    );
                }
            }
        }
        if self.kid.trim().is_empty() {
            return Err("[auth.jwt] kid 不能为空".to_string());
        }
        if self.access_ttl_secs <= 0 || self.refresh_ttl_secs <= 0 {
            return Err("[auth.jwt] access_ttl_secs / refresh_ttl_secs 必须 > 0".to_string());
        }
        Ok(())
    }
}

/// 账号体系归属。
///
/// - [`Builtin`](AccountMode::Builtin)：privchat-server 内置账号系统（独立部署 / 测试默认）
/// - [`Platform`](AccountMode::Platform)：账号体系托管于 privchat platform（如 privchat-application）；
///   server 端只承担 IM 通道职责，用户面 RPC 一律 forbidden。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AccountMode {
    Builtin,
    Platform,
}

impl Default for AccountMode {
    fn default() -> Self {
        AccountMode::Builtin
    }
}

/// `[account]` 配置块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub mode: AccountMode,
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            mode: AccountMode::Builtin,
        }
    }
}

impl AccountConfig {
    /// PLATFORM 模式下，server 端用户面 RPC（login/register/refresh）必须拒绝。
    pub fn is_builtin(&self) -> bool {
        self.mode == AccountMode::Builtin
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 9001,
            database_url: String::new(),
            database: DatabaseConfig::default(),
            max_connections: 1000,
            connection_timeout: 300,
            heartbeat_interval: 60,
            log_level: "info".to_string(),
            enable_tls: false,
            tls_cert_path: None,
            tls_key_path: None,
            cache: CacheConfig::default(),
            room: RoomConfig::default(),
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
            attachment_keys: AttachmentKeys::default(),
            dedup_master_key: DedupMasterKey::default(),
            http_file_server_host: "0.0.0.0".to_string(),
            admin_api_port: 9090,
            file_api_base_url: Some("http://localhost:9083/api/app".to_string()),
            account: AccountConfig::default(), // 默认 BUILTIN（独立部署 / 测试）
            system_message: SystemMessageConfig::default(),
            message: MessageConfig::default(),
            security: SecurityProtectionConfig::default(),
            handler_max_inflight: 2000,
            service_master_key: String::new(),
            redis_url: String::new(),
            push: PushConfig::default(),
            jwt: JwtConfig::default(),
            server_event: None,
            room_ticket: None,
            upload_token: None,
            qr_base_url: default_qr_base_url(),
            unauth_session_timeout_secs: default_unauth_session_timeout_secs(),
            unauth_cleanup_interval_secs: default_unauth_cleanup_interval_secs(),
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
        // 🔴 第二十轮：单一数据面没有阈值/回退。旧配置项出现即报错，
        // 防止管理员以为它还在生效。
        if toml_config
            .file
            .as_ref()
            .and_then(|f| f.s3_direct_threshold)
            .is_some()
        {
            anyhow::bail!("[file] s3_direct_threshold 已废止：单一数据面（配置单选）没有阈值/回退，请删除该配置项");
        }

        // 🔴 listener 级 tls_cert/tls_key 已废止，迁到网关级 [gateway.tls]。
        // 出现即报错，绝不"接受但忽略"——那正是这次要修的死配置 bug：字段解析了
        // 却从不生效，运维以为配了证书，服务端实际每次启动现生成临时自签证书，
        // SPKI 每次重启都变，客户端 pinning 全废。
        if let Some(listeners) = toml_config.gateway.as_ref().and_then(|g| g.listeners.as_ref()) {
            if let Some(l) = listeners
                .iter()
                .find(|l| l.tls_cert.is_some() || l.tls_key.is_some())
            {
                anyhow::bail!(
                    "[[gateway.listeners]] 的 tls_cert/tls_key 已废止（protocol=\"{}\"）：\
                     QUIC 与 TLS/TCP 必须共用同一套服务端身份，否则客户端按传输方式\
                     拿到不同 SPKI、pinning 失效。请改配网关级：\n\
                     \n[gateway.tls]\ncert = \"/etc/privchat/tls/server.crt\"\n\
                     key = \"/etc/privchat/tls/server.key\"",
                    l.protocol
                );
            }
        }

        ServerConfig::try_from(toml_config)
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
        if let Ok(value) = env::var("PRIVCHAT_DB_MAX_CONNECTIONS") {
            self.database.max_connections = value.parse().unwrap_or(self.database.max_connections);
        }
        if let Ok(value) = env::var("PRIVCHAT_DB_MIN_CONNECTIONS") {
            self.database.min_connections = value.parse().unwrap_or(self.database.min_connections);
        }
        if let Ok(value) = env::var("PRIVCHAT_DB_ACQUIRE_TIMEOUT_SECONDS") {
            self.database.acquire_timeout_seconds = value
                .parse()
                .unwrap_or(self.database.acquire_timeout_seconds);
        }
        if let Ok(value) = env::var("PRIVCHAT_DB_IDLE_TIMEOUT_SECONDS") {
            self.database.idle_timeout_seconds =
                value.parse().unwrap_or(self.database.idle_timeout_seconds);
        }
        if let Ok(value) = env::var("PRIVCHAT_DB_MAX_LIFETIME_SECONDS") {
            self.database.max_lifetime_seconds =
                value.parse().unwrap_or(self.database.max_lifetime_seconds);
        }
        if let Ok(value) = env::var("PRIVCHAT_DB_STATEMENT_TIMEOUT_MS") {
            self.database.statement_timeout_ms =
                value.parse().unwrap_or(self.database.statement_timeout_ms);
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

        // 统一 JWT 配置（spec TOKEN_UNIFICATION_SPEC v1.3 §4 / §6）
        if let Ok(algo) = env::var("PRIVCHAT_JWT_ALGORITHM") {
            match algo.trim().to_ascii_uppercase().as_str() {
                "HS256" => self.jwt.algorithm = JwtAlgorithm::Hs256,
                "RS256" => self.jwt.algorithm = JwtAlgorithm::Rs256,
                other => tracing::warn!(
                    "PRIVCHAT_JWT_ALGORITHM 非法值 '{}'（仅 HS256 / RS256），保留默认 {:?}",
                    other,
                    self.jwt.algorithm
                ),
            }
        }
        if let Ok(secret) = env::var("PRIVCHAT_JWT_SECRET") {
            self.jwt.secret = secret;
        }
        if let Ok(p) = env::var("PRIVCHAT_JWT_PRIVATE_KEY_PATH") {
            self.jwt.private_key_path = p;
        }
        if let Ok(p) = env::var("PRIVCHAT_JWT_PUBLIC_KEY_PATH") {
            self.jwt.public_key_path = p;
        }
        if let Ok(kid) = env::var("PRIVCHAT_JWT_KID") {
            if !kid.trim().is_empty() {
                self.jwt.kid = kid;
            }
        }
        if let Ok(ttl) = env::var("PRIVCHAT_JWT_ACCESS_TTL_SECS") {
            if let Ok(v) = ttl.parse::<i64>() {
                if v > 0 {
                    self.jwt.access_ttl_secs = v;
                }
            }
        }
        if let Ok(ttl) = env::var("PRIVCHAT_JWT_REFRESH_TTL_SECS") {
            if let Ok(v) = ttl.parse::<i64>() {
                if v > 0 {
                    self.jwt.refresh_ttl_secs = v;
                }
            }
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

        // 账号体系归属（BUILTIN | PLATFORM）
        if let Ok(mode) = env::var("PRIVCHAT_ACCOUNT_MODE") {
            match mode.trim().to_ascii_uppercase().as_str() {
                "BUILTIN" => self.account.mode = AccountMode::Builtin,
                "PLATFORM" => self.account.mode = AccountMode::Platform,
                other => tracing::warn!(
                    "PRIVCHAT_ACCOUNT_MODE 非法值 '{}'（仅支持 BUILTIN / PLATFORM），保留默认 {:?}",
                    other,
                    self.account.mode
                ),
            }
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

        // QR_CODE_SPEC v1.3 — 二维码 URL 基址环境覆盖
        if let Ok(qr_base_url) = env::var("PRIVCHAT_QR_BASE_URL") {
            self.qr_base_url = qr_base_url;
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
            self.jwt.secret = jwt_secret.clone();
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

        // 6. QR_CODE_SPEC v1.3 §7.2：qr_base_url 启动期 normalize（trim、去尾斜杠、
        //    scheme 校验、禁止预拼 /privchat:protocol）。生产环境强制 https。
        //    failure → server 拒启动。
        let require_https = matches!(cli.env.as_deref(), Some("production") | Some("prod"));
        config.qr_base_url =
            crate::rpc::qr::normalize_qr_base_url(&config.qr_base_url, require_https)
                .map_err(|e| anyhow::anyhow!("[qr_base_url] {}", e))?;

        Ok(config)
    }

    /// 校验必填配置项，缺失则报错退出
    fn validate(&self) -> Result<()> {
        let mut missing = Vec::new();

        if self.database_url.is_empty() {
            missing.push("DATABASE_URL");
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

        if self.database.max_connections == 0 {
            anyhow::bail!("[database] max_connections 必须大于 0");
        }
        if self.database.min_connections > self.database.max_connections {
            anyhow::bail!(
                "[database] min_connections ({}) 不能大于 max_connections ({})",
                self.database.min_connections,
                self.database.max_connections
            );
        }
        if self.database.acquire_timeout_seconds == 0 {
            anyhow::bail!("[database] acquire_timeout_seconds 必须大于 0");
        }

        // [auth.jwt] fail-fast 校验（算法 + 对应密钥）
        self.jwt
            .validate()
            .map_err(|e| anyhow::anyhow!("[auth.jwt] 配置错误: {}", e))?;

        Ok(())
    }
}

/// TOML 配置文件结构（用于反序列化）
#[derive(Debug, Deserialize)]
struct TomlConfig {
    gateway: Option<TomlGatewayConfig>,
    database: Option<TomlDatabaseConfig>,
    cache: Option<TomlCacheConfig>,
    room: Option<TomlRoomConfig>,
    file: Option<TomlFileConfig>,
    attachment: Option<TomlAttachmentConfig>,
    admin: Option<TomlAdminConfig>,
    auth: Option<TomlAuthConfig>,
    account: Option<TomlAccountConfig>,
    logging: Option<TomlLoggingConfig>,
    system_message: Option<TomlSystemMessageConfig>,
    message: Option<TomlMessageConfig>,
    push: Option<TomlPushConfig>,
    server_event: Option<TomlServerEventConfig>,
    room_ticket: Option<TomlRoomTicketConfig>,
    upload: Option<TomlUploadConfig>,
}

/// TOML `[server_event]` 段（spec 02-server/SERVER_EVENT_DISPATCH_SPEC §3）。
///
/// 通用 server→下游事件出站配置——所有 server 主动 emit 的 event（bot.followed /
/// channel.message_created / ...）共用这一份配置。
#[derive(Debug, Deserialize)]
struct TomlServerEventConfig {
    /// privchat-application（或任何下游订阅方）基址。
    application_url: Option<String>,
    /// 下游 master key；server 调下游时放入 `X-Service-Key` header。
    application_master_key: Option<String>,
    /// server → 下游 HTTP 调用超时，毫秒；缺省 3000。
    timeout_ms: Option<u64>,
}

/// TOML `[upload]` 段。
#[derive(Debug, Deserialize)]
struct TomlUploadConfig {
    token: Option<TomlUploadTokenConfig>,
}

/// TOML `[upload.token]` 段（spec foundation/RESUMABLE_UPLOAD_SPEC §5.2）。
///
/// 与 `[auth.jwt]` **是独立密钥域**：登录 token 与上传 token 互不通用。
#[derive(Debug, Deserialize)]
struct TomlUploadTokenConfig {
    /// 单 key 形式。
    secret: Option<String>,
    /// 多 key 形式：kid → secret。轮换期旧 key 必须保留 ≥ 24h + 时钟宽限，
    /// 否则一次换密钥会打断所有在途续传。
    #[serde(default)]
    keys: std::collections::HashMap<String, String>,
    default_kid: Option<String>,
    leeway_secs: Option<u64>,
    /// 签发有效期；缺省 24h，且不得超过硬上限。
    ttl_secs: Option<u64>,
    /// `legacy_uuid`（缺省）| `signed`。
    ///
    /// 🔴 **只控制签发的 token 格式**（Redis UUID vs 自包含签名），
    /// **不恢复旧语义**：两种格式都是 24 小时、可复用，整包路径同样受会话模式锁
    /// 与完成幂等约束。想回到「5 分钟 + 一次性消费」只能回滚版本。
    ///
    /// 验证侧始终双验，与本开关无关。配置无热更（`ServerConfig::load` 只在启动时
    /// 跑一次），切换需**改配置 + 重启服务**。
    issue_mode: Option<String>,
}

/// TOML `[room_ticket]` 段（spec 02-server/ROOM_CHANNEL_SPEC §4）
#[derive(Debug, Deserialize)]
struct TomlRoomTicketConfig {
    /// 单 key 形式：HMAC secret（base64 / 任意字符串均可，不解码）
    secret: Option<String>,
    /// 多 key 形式：kid → secret
    #[serde(default)]
    keys: std::collections::HashMap<String, String>,
    /// header.kid 未指定时使用的 key id；缺省 "v1"
    default_kid: Option<String>,
    /// 时钟容忍（秒）；缺省 30
    leeway_secs: Option<u64>,
}

/// TOML [auth] 段（spec TOKEN_UNIFICATION_SPEC v1.3 Phase A）
#[derive(Debug, Deserialize)]
struct TomlAuthConfig {
    jwt: Option<TomlJwtConfig>,
}

/// TOML `[auth.jwt]` 段（统一 token 配置）
#[derive(Debug, Deserialize)]
struct TomlJwtConfig {
    algorithm: Option<JwtAlgorithm>,
    secret: Option<String>,
    private_key_path: Option<String>,
    public_key_path: Option<String>,
    kid: Option<String>,
    access_ttl_secs: Option<i64>,
    refresh_ttl_secs: Option<i64>,
    issuer: Option<String>,
    default_audience: Option<Vec<String>>,
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

/// TOML `[attachment]` 段：附件加密密钥。
#[derive(Debug, Deserialize)]
struct TomlAttachmentConfig {
    /// `[[attachment.keys]]`，第一项为当前使用的密钥。
    keys: Option<Vec<TomlAttachmentKey>>,
    /// `[attachment] dedup_master_key`：base64url(no-pad) 的 32 字节。
    dedup_master_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TomlAttachmentKey {
    id: u8,
    /// base64url(no-pad) 的 32 字节密钥。
    key: String,
}

/// TOML `[gateway.tls]` 段：网关级 TLS 身份。
#[derive(Debug, Deserialize)]
struct TomlGatewayTlsConfig {
    cert: String,
    key: String,
}

/// TOML 单条 listener 反序列化
#[derive(Debug, Deserialize)]
struct TomlListenerConfig {
    protocol: String,
    #[serde(default = "default_listener_host")]
    host: String,
    port: u16,
    /// 已废止：保留仅为在 from_toml_file 里检测到旧配置时明确报错，见那里的注释。
    tls_cert: Option<String>,
    /// 已废止，同上。
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
            path: None,
            compression: None,
            internal: None,
        },
        GatewayListenerConfig {
            protocol: "quic".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9001,
            bind_address: None,
            path: None,
            compression: None,
            internal: None,
        },
        GatewayListenerConfig {
            protocol: "websocket".to_string(),
            host: "0.0.0.0".to_string(),
            port: 9080,
            bind_address: None,
            path: None,
            compression: None,
            internal: None,
        },
    ]
}

/// 网关配置（TCP/WebSocket/QUIC）；gateway.listeners 为多监听入口
#[derive(Debug, Deserialize)]
struct TomlGatewayConfig {
    /// 网关级 TLS 身份：QUIC 与 TLS/TCP **共用同一套长期密钥和同一组 SPKI pins**
    /// （GATEWAY_TRANSPORT_SPEC §1.1）。放在网关级而不是 listener 级，是为了从结构上
    /// 杜绝「两个 listener 配出不同身份」——那会让客户端按传输方式拿到不同 SPKI，
    /// pinning 直接失效。
    tls: Option<TomlGatewayTlsConfig>,
    /// 多监听入口：每项 protocol + host + port
    listeners: Option<Vec<TomlListenerConfig>>,
    max_connections: Option<u32>,
    connection_timeout: Option<u64>,
    heartbeat_interval: Option<u64>,
    handler_max_inflight: Option<usize>,
}

/// TOML `[database]` 段。
#[derive(Debug, Deserialize)]
struct TomlDatabaseConfig {
    /// 数据库连接字符串；也可继续通过 DATABASE_URL 环境变量覆盖。
    url: Option<String>,
    max_connections: Option<u32>,
    min_connections: Option<u32>,
    acquire_timeout_seconds: Option<u64>,
    idle_timeout_seconds: Option<u64>,
    max_lifetime_seconds: Option<u64>,
    /// PostgreSQL statement_timeout，0 表示禁用。
    statement_timeout_ms: Option<u64>,
}

/// TOML `[account]` 段。
#[derive(Debug, Deserialize)]
struct TomlAccountConfig {
    mode: Option<AccountMode>,
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

#[derive(Debug, Deserialize)]
struct TomlRoomConfig {
    subscribe_history: Option<bool>,
    subscribe_history_limit: Option<usize>,
    history_ttl_seconds: Option<usize>,
    max_subscriptions_per_session: Option<usize>,
    max_channel_subscribers_online: Option<usize>,
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
    /// S3 直传显式开关（RESUMABLE §8.2）：目前唯一合法值 `"s3_multipart_v1"`，
    /// 默认不开。🔴 不能以 `storage_type = "s3"` 直接判定可直传：各 S3 兼容后端在
    /// 预签名/checksum/条件写行为上不完全一致，必须显式开启并过集成门禁。
    #[serde(default)]
    pub direct_upload: Option<String>,
    /// SigV4 签名用的 region（仅 direct_upload 开启时需要；未填默认 us-east-1，
    /// 自建 MinIO/Garage 一般任意值可过）。
    #[serde(default)]
    pub region: Option<String>,
    /// 🔴 第二十八轮：S3 寻址方式。不填或 `"path"` = path-style
    /// `{endpoint}/{bucket}/{key}`（自建 MinIO/Garage）；`"virtual"` = 虚拟主机寻址
    /// `{scheme}://{bucket}.{host}/{key}`——腾讯云 COS 明确禁止 path-style
    /// （PathStyleDomainForbidden），必须显式配 virtual。其他值启动期报错（fail-fast）。
    #[serde(default)]
    pub addressing_style: Option<String>,
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
    /// 监听地址；nginx 终结 TLS 时填 "127.0.0.1"。
    server_host: Option<String>,
    /// 文件服务 API 基础 URL，客户端访问（原 file_server.api_base_url）
    server_api_base_url: Option<String>,
    /// 🔴 第二十轮已废止：单一数据面没有阈值/回退。旧配置里出现即报错，
    /// 防止管理员以为它还在生效。
    s3_direct_threshold: Option<u64>,
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
    #[serde(default)]
    direct_upload: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    addressing_style: Option<String>,
}

/// TOML [logging] 段，用于反序列化
#[derive(Debug, Deserialize)]
struct TomlLoggingConfig {
    level: Option<String>,
    format: Option<String>,
    file: Option<String>,
    /// 归档日志保留天数；缺省 7，0 = 不清理
    retention_days: Option<u32>,
}

/// 早期日志配置（在完整 ServerConfig 加载之前，快速读取 [logging] 段）
#[derive(Debug, Default)]
pub struct EarlyLoggingConfig {
    pub level: Option<String>,
    pub format: Option<String>,
    pub file: Option<String>,
    /// 归档日志保留天数；None = 用 `logging::DEFAULT_LOG_RETENTION_DAYS`
    pub retention_days: Option<u32>,
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
            retention_days: log.retention_days,
        },
        None => EarlyLoggingConfig::default(),
    }
}

/// `[attachment]` 段的**唯一**校验与解码入口。
///
/// 🔴 这些规则曾经散落在两条路径上：`from_toml_file` 返回 `Result`，
/// `From<TomlConfig>` 里 panic，而且两边规则并不一样——一边查了成对性和密钥相同，
/// 另一边只查编码。任何一条构造路径漏掉一条规则，都会把配置错误变成
/// "跨用户秒传悄悄关掉"，而运行期毫无迹象。
fn attachment_material_from_toml(
    att: Option<&TomlAttachmentConfig>,
) -> Result<(AttachmentKeys, DedupMasterKey)> {
    use base64::Engine as _;

    fn decode_32(raw: &str, what: &str) -> Result<Vec<u8>> {
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw.trim().as_bytes())
            .map_err(|_| anyhow::anyhow!("{what} 不是合法 base64url(no-pad)"))?;
        if decoded.len() != 32 {
            anyhow::bail!("{what} 解码后必须是 32 字节，实际 {}", decoded.len());
        }
        Ok(decoded)
    }

    let keys = att.and_then(|a| a.keys.as_ref());
    let dedup_raw = att
        .and_then(|a| a.dedup_master_key.as_deref())
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let has_keys = keys.is_some_and(|k| !k.is_empty());

    // 🔴 配了一半就拒绝启动，不是静默降级。缺 dedup 密钥时跨用户秒传永远不命中，
    // 而运行期不会有任何报错——用户只看到每次发同一张图都在重新上传。
    if has_keys && dedup_raw.is_none() {
        anyhow::bail!(
            "[attachment] 配置了 [[attachment.keys]] 就必须同时配 dedup_master_key：\
             缺了它跨用户秒传永远不命中，而运行期不会有任何报错"
        );
    }
    if dedup_raw.is_some() && !has_keys {
        anyhow::bail!(
            "[attachment] 配了 dedup_master_key 却没有 [[attachment.keys]]：\
             没有加密就没有附件对象要去重，这多半是配置写漏了一半"
        );
    }

    let mut decoded_keys: Vec<(u8, Vec<u8>)> = Vec::new();
    if let Some(keys) = keys {
        for k in keys {
            // key_id 重复会让密文头的自描述失效：两代密钥指向同一个 id，
            // 老对象再也解不开。
            if decoded_keys.iter().any(|(id, _)| *id == k.id) {
                anyhow::bail!("[[attachment.keys]] key_id 重复: {}", k.id);
            }
            let material = decode_32(&k.key, &format!("[[attachment.keys]] id={} 的 key", k.id))?;
            // 同一把密钥挂两个 id 等于假轮换：换了 id 却没换密钥。
            if decoded_keys.iter().any(|(_, m)| *m == material) {
                anyhow::bail!("[[attachment.keys]] 存在重复的密钥内容，轮换无效");
            }
            decoded_keys.push((k.id, material));
        }
    }

    let dedup = match dedup_raw {
        None => DedupMasterKey::default(),
        Some(raw) => {
            let decoded = decode_32(raw, "[attachment] dedup_master_key")?;
            // 🔴 与任何一把加密密钥相同就等于"从加密密钥派生"：加密密钥一轮换，
            // 运维很可能顺手把这里也换掉，全站秒传索引当场作废。
            //
            // 比的是解码后的 32 字节，不是 base64 串——编码写法上的一点差异
            // （多一个空格、换一种 padding 习惯）会让字符串比较判为不同，
            // 而底下是同一把密钥，那正是这条检查要拦的情况。
            if decoded_keys.iter().any(|(_, m)| *m == decoded) {
                anyhow::bail!(
                    "[attachment] dedup_master_key 不能与 [[attachment.keys]] 中任何一把相同：\
                     它必须独立于可轮换的加密密钥，否则轮换会让历史对象再也无法秒传命中"
                );
            }
            DedupMasterKey(Some(decoded))
        }
    };

    // 密钥表保留原始 base64 形态：它要原样下发给客户端。
    let table = AttachmentKeys(
        keys.map(|k| k.iter().map(|k| (k.id, k.key.trim().to_string())).collect())
            .unwrap_or_default(),
    );
    Ok((table, dedup))
}

impl TryFrom<TomlConfig> for ServerConfig {
    type Error = anyhow::Error;

    fn try_from(toml: TomlConfig) -> Result<Self> {
        let mut config = Self::default();

        // 网关：gateway.listeners
        // 无论从哪条路径构造，附件密钥都走同一套规则。
        let (keys, dedup) = attachment_material_from_toml(toml.attachment.as_ref())?;
        config.attachment_keys = keys;
        config.dedup_master_key = dedup;

        if let Some(gw) = toml.gateway {
            // 网关级 TLS 身份，QUIC 与 TLS/TCP 共用。此前 listener 上的
            // tls_cert/tls_key 解析了却从不传递，顶层 tls_cert_path 是死字段，
            // 服务端于是每次启动现生成自签证书、SPKI 每次都变，客户端无法 pin。
            if let Some(tls) = gw.tls {
                config.tls_cert_path = Some(tls.cert);
                config.tls_key_path = Some(tls.key);
            }
            if let Some(max_conn) = gw.max_connections {
                config.max_connections = max_conn;
            }
            if let Some(timeout) = gw.connection_timeout {
                config.connection_timeout = timeout;
            }
            if let Some(interval) = gw.heartbeat_interval {
                config.heartbeat_interval = interval;
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

        if let Some(database) = toml.database {
            if let Some(url) = database.url {
                config.database_url = url;
            }
            if let Some(max_connections) = database.max_connections {
                config.database.max_connections = max_connections;
            }
            if let Some(min_connections) = database.min_connections {
                config.database.min_connections = min_connections;
            }
            if let Some(acquire_timeout_seconds) = database.acquire_timeout_seconds {
                config.database.acquire_timeout_seconds = acquire_timeout_seconds;
            }
            if let Some(idle_timeout_seconds) = database.idle_timeout_seconds {
                config.database.idle_timeout_seconds = idle_timeout_seconds;
            }
            if let Some(max_lifetime_seconds) = database.max_lifetime_seconds {
                config.database.max_lifetime_seconds = max_lifetime_seconds;
            }
            if let Some(statement_timeout_ms) = database.statement_timeout_ms {
                config.database.statement_timeout_ms = statement_timeout_ms;
            }
        }

        if let Some(room) = toml.room {
            if let Some(subscribe_history) = room.subscribe_history {
                config.room.subscribe_history = subscribe_history;
            }
            if let Some(subscribe_history_limit) = room.subscribe_history_limit {
                config.room.subscribe_history_limit = subscribe_history_limit;
            }
            if let Some(history_ttl_seconds) = room.history_ttl_seconds {
                config.room.history_ttl_seconds = history_ttl_seconds;
            }
            if let Some(max_subscriptions_per_session) = room.max_subscriptions_per_session {
                config.room.max_subscriptions_per_session = max_subscriptions_per_session;
            }
            if let Some(max_channel_subscribers_online) = room.max_channel_subscribers_online {
                config.room.max_channel_subscribers_online = max_channel_subscribers_online;
            }
        }

        if let Some(file) = toml.file {
            if let Some(host) = file.server_host.clone() {
                config.http_file_server_host = host;
            }
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
                        direct_upload: s.direct_upload,
                        region: s.region,
                        addressing_style: s.addressing_style,
                    })
                    .collect();
            }
            if let Some(id) = file.default_storage_source_id {
                config.file_default_storage_source_id = id;
            }
            // s3_direct_threshold 已废止：在 from_toml_file 入口报错，这里不再读取。
        }

        if let Some(admin) = toml.admin {
            if let Some(port) = admin.port {
                config.admin_api_port = port;
            }
            if let Some(key) = admin.master_key {
                config.service_master_key = key;
            }
        }

        if let Some(auth) = toml.auth {
            if let Some(jwt) = auth.jwt {
                if let Some(algo) = jwt.algorithm {
                    config.jwt.algorithm = algo;
                }
                if let Some(s) = jwt.secret {
                    config.jwt.secret = s;
                }
                if let Some(p) = jwt.private_key_path {
                    config.jwt.private_key_path = p;
                }
                if let Some(p) = jwt.public_key_path {
                    config.jwt.public_key_path = p;
                }
                if let Some(kid) = jwt.kid {
                    if !kid.trim().is_empty() {
                        config.jwt.kid = kid;
                    }
                }
                if let Some(ttl) = jwt.access_ttl_secs {
                    if ttl > 0 {
                        config.jwt.access_ttl_secs = ttl;
                    }
                }
                if let Some(ttl) = jwt.refresh_ttl_secs {
                    if ttl > 0 {
                        config.jwt.refresh_ttl_secs = ttl;
                    }
                }
                if let Some(iss) = jwt.issuer {
                    if !iss.trim().is_empty() {
                        config.jwt.issuer = iss;
                    }
                }
                if let Some(aud) = jwt.default_audience {
                    if !aud.is_empty() {
                        config.jwt.default_audience = aud;
                    }
                }
            }
        }

        if let Some(account) = toml.account {
            if let Some(mode) = account.mode {
                config.account.mode = mode;
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

        if let Some(message) = toml.message {
            if let Some(limit) = message.recall_time_limit_secs {
                // 允许 0 表示不限制时效；负数归一化为 0。
                config.message.recall_time_limit_secs = limit.max(0);
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

        // [server_event]：require both URL and master key to enable.
        // Missing or partial config keeps `server_event = None`, which means
        // all server→downstream emit (transfer.requested / bot.followed / ...)
        // is skipped (spec §6 best-effort fire-and-forget)，且 wire
        // `TransferRequest` ingress handler 不会注册（缺下游可投递）。
        if let Some(se) = toml.server_event {
            if let (Some(url), Some(key)) = (se.application_url, se.application_master_key) {
                if !url.is_empty() && !key.is_empty() {
                    config.server_event = Some(ServerEventConfig {
                        application_url: url,
                        application_master_key: key,
                        timeout_ms: se.timeout_ms.unwrap_or(3000),
                    });
                }
            }
        }

        // [room_ticket]: must have at least one key (`secret` or non-empty `keys`)
        // to take effect. Otherwise Room subscribe falls back to "authenticated only"
        // (no ticket verification — v1 compat mode).
        if let Some(rt) = toml.room_ticket {
            let has_secret = rt.secret.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
            let has_keys = !rt.keys.is_empty();
            if has_secret || has_keys {
                let leeway = rt.leeway_secs.unwrap_or(30).min(300);
                config.room_ticket = Some(RoomTicketConfig {
                    secret: rt.secret.filter(|s| !s.is_empty()),
                    keys: rt.keys,
                    default_kid: rt.default_kid.unwrap_or_else(|| "v1".to_string()),
                    leeway_secs: leeway,
                });
            }
        }

        // [upload.token]：至少要有一个密钥才生效；没有密钥就签不了也验不了签名 token，
        // 此时只剩旧 UUID 路径（与今天行为一致）。
        if let Some(up) = toml.upload {
            if let Some(t) = up.token {
                let mut keys = t.keys;
                if let Some(secret) = t.secret.filter(|s| !s.is_empty()) {
                    keys.entry(
                        t.default_kid.clone().unwrap_or_else(|| "upload-v1".to_string()),
                    )
                    .or_insert(secret);
                }
                if !keys.is_empty() {
                    use crate::security::upload_token::{IssueMode, MAX_TTL_SECS};
                    let issue_mode = match t.issue_mode.as_deref() {
                        Some("signed") => IssueMode::Signed,
                        // 🔴 认不出的值一律退回 legacy：配置写错不该顺便把 token
                        // 格式切了。
                        _ => IssueMode::LegacyUuid,
                    };
                    config.upload_token =
                        Some(crate::security::upload_token::UploadTokenConfig {
                            keys,
                            default_kid: t
                                .default_kid
                                .unwrap_or_else(|| "upload-v1".to_string()),
                            leeway_secs: t.leeway_secs.unwrap_or(30).min(300),
                            ttl_secs: t.ttl_secs.unwrap_or(MAX_TTL_SECS).min(MAX_TTL_SECS),
                            issue_mode,
                        });
                }
            }
        }

        Ok(config)
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

/// Server Event 出站配置（spec 02-server/SERVER_EVENT_DISPATCH_SPEC §3）。
///
/// 通用 server→下游事件出站配置——所有 server 主动 emit 的 event（含
/// wire `TransferRequest` 包装的 `transfer.requested` + `bot.followed` / ...）
/// 共用这一份配置，endpoint 固定走 `/service/privchat/server-event/dispatch`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEventConfig {
    /// 下游订阅方基址（不含路径），通常 = privchat-application。
    pub application_url: String,
    /// 下游的 master key；放入 `X-Service-Key` header。
    pub application_master_key: String,
    /// HTTP 调用超时（毫秒）；缺省 3000，与 transfer dispatch 一致。
    pub timeout_ms: u64,
}

/// Room subscribe ticket 校验配置
/// （spec 02-server/ROOM_CHANNEL_SPEC §4）。
///
/// gateway 用 `secret` 做 HMAC-SHA256 verify。多 key 支持通过 `keys` map 实现：
/// JWT header 的 `kid` 指明哪把 key，缺省走 `default_kid` 那把。`secret` 仍保留
/// 作为兼容字段（等价于单 key + `default_kid=v1`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomTicketConfig {
    /// 单 key 形式：HMAC secret。与 `keys` 二选一；同时配则 `keys` 优先。
    /// header.kid 未指定时也用这个值（视作 default key）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,

    /// 多 key 形式：kid → secret。轮换时旧 key 仍可校验已签发但未过期的 ticket。
    #[serde(default)]
    pub keys: std::collections::HashMap<String, String>,

    /// header.kid 未指定时使用的 key id。配合 `keys` 用；缺省 `"v1"`。
    #[serde(default = "default_kid")]
    pub default_kid: String,

    /// 时钟容忍（秒）；签发时钟漂移容差。建议 30，最大 300。
    #[serde(default = "default_leeway_secs")]
    pub leeway_secs: u64,
}

fn default_kid() -> String {
    "v1".to_string()
}

fn default_leeway_secs() -> u64 {
    30
}

impl RoomTicketConfig {
    /// 查找 kid 对应的 secret。kid=None 时用 default_kid。
    /// 返回 `None` = 该 kid 在配置中不存在。
    pub fn resolve_secret(&self, kid: Option<&str>) -> Option<&str> {
        let kid_lookup = kid.unwrap_or(self.default_kid.as_str());
        if let Some(s) = self.keys.get(kid_lookup) {
            return Some(s.as_str());
        }
        // 单 key 兼容：keys 为空且使用 default_kid 时回退到顶层 secret
        if kid_lookup == self.default_kid {
            return self.secret.as_deref();
        }
        None
    }
}

/// Room 订阅配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomConfig {
    /// 新订阅时是否自动推送近期历史
    pub subscribe_history: bool,
    /// 新订阅时推送的历史条数上限
    pub subscribe_history_limit: usize,
    /// Room recent buffer 的 Redis TTL（秒）；0 表示不设置 TTL。
    pub history_ttl_seconds: usize,
    /// 每个 session 最多同时订阅的频道数。
    pub max_subscriptions_per_session: usize,
    /// 单个 room/channel 最大在线订阅 session 数。
    pub max_channel_subscribers_online: usize,
}

impl Default for RoomConfig {
    fn default() -> Self {
        Self {
            subscribe_history: true,
            subscribe_history_limit: 30,
            history_ttl_seconds: 86_400,
            max_subscriptions_per_session: 32,
            max_channel_subscribers_online: 20_000,
        }
    }
}

/// 数据库连接池与查询保护配置
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// 连接池最大连接数
    pub max_connections: u32,
    /// 连接池最小连接数
    pub min_connections: u32,
    /// 从连接池获取连接的超时时间（秒）
    pub acquire_timeout_seconds: u64,
    /// 空闲连接回收时间（秒）
    pub idle_timeout_seconds: u64,
    /// 单连接最大生命周期（秒）
    pub max_lifetime_seconds: u64,
    /// PostgreSQL statement_timeout（毫秒）；0 表示禁用。
    pub statement_timeout_ms: u64,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            max_connections: 20,
            min_connections: 5,
            acquire_timeout_seconds: 10,
            idle_timeout_seconds: 600,
            max_lifetime_seconds: 1800,
            statement_timeout_ms: 5000,
        }
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
            cleanup_interval_secs: 30, // 30秒扫描一次超时，确保快速检测离线状态
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
// 消息配置（撤回时效等）
// =====================================================

/// 消息相关配置。
///
/// 目前承载"撤回时效"——普通用户撤回自己消息的时间窗口。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageConfig {
    /// 普通用户撤回自己消息的时效（秒）。群主/管理员不受限。
    ///
    /// - `> 0`：普通用户仅能在发送后该秒数内撤回
    /// - `0`（或配置为 null / 省略）：**不限制时效**，普通用户任意时间均可撤回自己的消息
    #[serde(default = "default_recall_time_limit_secs")]
    pub recall_time_limit_secs: i64,
}

fn default_recall_time_limit_secs() -> i64 {
    // 默认 48h，保持历史行为；运营可在 [message] 段改为 0 表示不限制。
    172800
}

impl Default for MessageConfig {
    fn default() -> Self {
        Self {
            recall_time_limit_secs: default_recall_time_limit_secs(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct TomlMessageConfig {
    recall_time_limit_secs: Option<i64>,
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

/// 生成默认配置文件**及其 TLS 证书**。
///
/// 官方生成命令必须产出一个能真正跑起来的最小环境。只写配置是不够的：
/// 服务端对 TLS 材料无条件 fail-closed，配置里写了 `[gateway.tls]` 而证书
/// 不存在，`--generate-config` 的产物就是启动即失败。
///
/// 模板源是清理过的 `config.example.toml`，不是仓库里的 `config.toml`——
/// 后者带着本地地址与开发密钥（room ticket secret、JWT 路径、localhost
/// Redis），不能作为生成源。
pub fn generate_config_with_tls(path: &str) -> Result<()> {
    let template = include_str!("../config.example.toml");

    // 🔴 每个占位密钥都换成**本次生成的随机值**。
    // 模板里发一个固定的 CHANGE_ME 出去，等于所有按官方命令部署的实例共用
    // 同一把密钥——那不是"可启动的最小环境"，是把签名密钥公开在仓库里。
    let rendered = template
        .replace("CHANGE_ME_jwt_hs256_secret", &random_secret())
        .replace("CHANGE_ME_room_ticket_hmac_secret", &random_secret())
        .replace("CHANGE_ME_service_master_key", &random_secret());
    fs::write(path, rendered).with_context(|| format!("无法写入配置文件: {}", path))?;
    println!("✅ 配置文件已生成: {}", path);

    // 证书路径相对配置文件所在目录，与模板里的 `./certs/...` 对应。
    let base = Path::new(path).parent().unwrap_or_else(|| Path::new("."));
    let cert_dir = base.join("certs");
    let cert_path = cert_dir.join("server.crt");
    let key_path = cert_dir.join("server.key");

    if cert_path.exists() || key_path.exists() {
        println!("ℹ️  已存在证书，保留不覆盖: {}", cert_dir.display());
        println!("   覆盖会让所有已发布客户端的 SPKI pin 失效。");
        return Ok(());
    }

    fs::create_dir_all(&cert_dir)
        .with_context(|| format!("无法创建证书目录: {}", cert_dir.display()))?;

    let (cert_pem, key_pem) = generate_long_lived_self_signed("localhost")?;
    fs::write(&cert_path, &cert_pem)
        .with_context(|| format!("无法写入证书: {}", cert_path.display()))?;
    fs::write(&key_path, &key_pem)
        .with_context(|| format!("无法写入私钥: {}", key_path.display()))?;

    // 私钥必须 0600：服务端启动时会拒绝 group/world 可读的私钥。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("无法设置私钥权限: {}", key_path.display()))?;
    }

    println!("🔐 已生成长期自签证书 (10 年):");
    println!("   {}", cert_path.display());
    println!("   {} (0600)", key_path.display());
    println!("⚠️  证书 CN/SAN 为 localhost，仅供本地起服务。");
    println!("   对外部署请改用真实地址重新生成：");
    println!("   ./scripts/gen-server-tls.sh <host-or-ip> <outdir>");
    Ok(())
}

/// 32 字节 CSPRNG 随机数的十六进制串，用作生成配置里的对称密钥。
fn random_secret() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 生成一张 10 年期自签证书，返回 (cert_pem, key_pem)。
fn generate_long_lived_self_signed(cn: &str) -> Result<(String, String)> {
    let key = rcgen::KeyPair::generate().context("生成密钥对失败")?;
    let mut params = rcgen::CertificateParams::new(vec![cn.to_string()])
        .with_context(|| format!("证书参数无效: {cn}"))?;
    params.not_after = rcgen::date_time_ymd(2036, 1, 1);
    let cert = params.self_signed(&key).context("签发自签证书失败")?;
    Ok((cert.pem(), key.serialize_pem()))
}


#[cfg(test)]
mod legacy_listener_tls_tests {
    use super::ServerConfig;
    use std::io::Write;

    fn write_cfg(body: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().expect("tmp");
        f.write_all(body.as_bytes()).expect("write");
        f.flush().expect("flush");
        f
    }

    /// 旧的 listener 级证书配置必须**拒绝启动**，不能"接受但忽略"——
    /// 那正是这次修的死配置 bug：运维以为配了证书，实际服务端每次启动
    /// 现生成临时自签证书，SPKI 每次重启都变，客户端 pinning 全废。
    #[test]
    fn legacy_listener_tls_cert_is_rejected() {
        let f = write_cfg(
            r#"
[[gateway.listeners]]
protocol = "quic"
host = "0.0.0.0"
port = 9001
tls_cert = "/etc/privchat/tls/server.crt"
"#,
        );
        let err = ServerConfig::from_toml_file(f.path()).expect_err("legacy field must be rejected");
        let msg = format!("{err:?}");
        assert!(msg.contains("已废止"), "{msg}");
        assert!(msg.contains("[gateway.tls]"), "{msg}");
    }

    #[test]
    fn legacy_listener_tls_key_is_rejected() {
        let f = write_cfg(
            r#"
[[gateway.listeners]]
protocol = "tcp"
host = "0.0.0.0"
port = 9001
tls_key = "/etc/privchat/tls/server.key"
"#,
        );
        assert!(ServerConfig::from_toml_file(f.path()).is_err());
    }

    /// 网关级配置是正路，必须能正常读到。
    #[test]
    fn gateway_level_tls_is_accepted() {
        let f = write_cfg(
            r#"
[gateway.tls]
cert = "/etc/privchat/tls/server.crt"
key = "/etc/privchat/tls/server.key"

[[gateway.listeners]]
protocol = "quic"
host = "0.0.0.0"
port = 9001
"#,
        );
        let cfg = ServerConfig::from_toml_file(f.path()).expect("should parse");
        assert_eq!(
            cfg.tls_cert_path.as_deref(),
            Some("/etc/privchat/tls/server.crt")
        );
        assert_eq!(
            cfg.tls_key_path.as_deref(),
            Some("/etc/privchat/tls/server.key")
        );
    }
}

#[cfg(test)]
mod generate_config_tests {
    use super::{generate_config_with_tls, ServerConfig};

    /// `--generate-config` 必须产出一个**能真正启动**的最小环境。
    /// 只写配置是不够的：服务端对 TLS 材料无条件 fail-closed，配置里写了
    /// [gateway.tls] 而证书不存在，产物就是启动即失败。这条测试一路验到
    /// 证书能被 server 侧的加载器接受为止。
    #[test]
    fn generated_environment_is_actually_startable() {
        let dir = tempfile::tempdir().expect("tmp");
        let cfg_path = dir.path().join("config.toml");
        generate_config_with_tls(cfg_path.to_str().unwrap()).expect("generate");

        let cfg = ServerConfig::from_toml_file(&cfg_path).expect("生成的配置必须能解析");
        let (cert_path, key_path) = (
            cfg.tls_cert_path.expect("必须带出证书路径"),
            cfg.tls_key_path.expect("必须带出私钥路径"),
        );

        // 模板里的路径是相对配置文件所在目录的
        let cert_abs = dir.path().join(cert_path.trim_start_matches("./"));
        let key_abs = dir.path().join(key_path.trim_start_matches("./"));
        assert!(cert_abs.exists(), "证书未生成: {}", cert_abs.display());
        assert!(key_abs.exists(), "私钥未生成: {}", key_abs.display());

        // 关键一步：服务端启动时用的就是这个加载器（真解析 + 真配对 + 权限检查）
        crate::server::load_tls_material(
            Some(cert_abs.to_str().unwrap()),
            Some(key_abs.to_str().unwrap()),
        )
        .expect("生成的证书必须能通过服务端启动校验");
    }

    #[cfg(unix)]
    #[test]
    fn generated_private_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tmp");
        let cfg_path = dir.path().join("config.toml");
        generate_config_with_tls(cfg_path.to_str().unwrap()).expect("generate");
        let mode = std::fs::metadata(dir.path().join("certs/server.key"))
            .expect("key")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "私钥权限必须是 0600，实际 {mode:04o}");
    }

    /// 重复执行不得覆盖已有证书——覆盖会让所有已发布客户端的 SPKI pin 失效。
    #[test]
    fn regenerating_keeps_existing_certificate() {
        let dir = tempfile::tempdir().expect("tmp");
        let cfg_path = dir.path().join("config.toml");
        generate_config_with_tls(cfg_path.to_str().unwrap()).expect("generate 1");
        let first = std::fs::read_to_string(dir.path().join("certs/server.crt")).unwrap();
        generate_config_with_tls(cfg_path.to_str().unwrap()).expect("generate 2");
        let second = std::fs::read_to_string(dir.path().join("certs/server.crt")).unwrap();
        assert_eq!(first, second, "重复生成不得覆盖证书");
    }

    /// 模板源必须是清理过的 config.example.toml，不能把开发密钥带进产物。
    #[test]
    fn generated_config_carries_no_development_secrets() {
        let dir = tempfile::tempdir().expect("tmp");
        let cfg_path = dir.path().join("config.toml");
        generate_config_with_tls(cfg_path.to_str().unwrap()).expect("generate");
        let body = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!body.contains("dev-room-ticket-secret"), "带入了开发 room ticket secret");
        assert!(!body.contains("your_service_master_key_here"));
    }

    /// 🔴 密钥必须逐次随机。发一个固定占位值出去，等于所有按官方命令部署的
    /// 实例共用同一把签名密钥。
    #[test]
    fn generated_secrets_are_random_per_invocation() {
        let read = |p: &std::path::Path| std::fs::read_to_string(p).unwrap();
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        let p1 = d1.path().join("config.toml");
        let p2 = d2.path().join("config.toml");
        generate_config_with_tls(p1.to_str().unwrap()).unwrap();
        generate_config_with_tls(p2.to_str().unwrap()).unwrap();
        let (a, b) = (read(&p1), read(&p2));

        assert!(!a.contains("CHANGE_ME"), "生成产物不得留下固定占位密钥");
        for key in ["secret", "application_master_key"] {
            let line_a = a.lines().find(|l| l.trim_start().starts_with(key)).unwrap();
            let line_b = b.lines().find(|l| l.trim_start().starts_with(key)).unwrap();
            assert_ne!(line_a, line_b, "{key} 在两次生成之间必须不同");
        }
        // 64 个十六进制字符 = 32 字节熵
        assert!(a.contains(&"0".repeat(0)) && a.lines().any(|l| {
            l.contains("secret") && l.split('"').nth(1).map(|v| v.len() == 64).unwrap_or(false)
        }));
    }
}

#[cfg(test)]
mod dedup_master_key_tests {
    use super::DedupMasterKey;

    fn key() -> DedupMasterKey {
        DedupMasterKey(Some(vec![7u8; 32]))
    }

    /// 秒传的全部前提：同一份明文永远算出同一个 dedup_id。
    /// 这里钉死一个 fixture——换了 info 串、换了输入编码（比如误把明文摘要转成
    /// 十六进制再喂进去）都不会报错，只会让全站秒传静默失效。
    #[test]
    fn the_dedup_id_is_a_frozen_function_of_the_plaintext_digest() {
        let digest = [0xabu8; 32];
        assert_eq!(
            key().dedup_id(&digest).expect("configured"),
            // 独立算过的值（python: hmac.new(b"\x07"*32, b"privchat-attachment-dedup-v1"+b"\xab"*32, sha256)），
            // 不是把实现的输出抄回来——抄回来的 fixture 只能证明实现没变，证明不了它算对了。
            "922b6965c25bf988696dcf7858fae43c62141cb6f1d62fca3eb7252f89bb4b26"
        );
    }

    #[test]
    fn different_content_yields_a_different_dedup_id() {
        let k = key();
        assert_ne!(k.dedup_id(&[1u8; 32]), k.dedup_id(&[2u8; 32]));
    }

    /// 🔴 换一把 dedup 密钥，同一份明文就换一个 dedup_id——这正是它绝不能跟着
    /// 加密密钥一起轮换的原因：轮换一次，全部历史对象的秒传索引作废。
    #[test]
    fn rotating_the_key_invalidates_every_existing_dedup_id() {
        let digest = [0xabu8; 32];
        let other = DedupMasterKey(Some(vec![8u8; 32]));
        assert_ne!(key().dedup_id(&digest), other.dedup_id(&digest));
    }

    /// 没配就没有跨用户秒传，而不是退回某个默认密钥。
    #[test]
    fn an_absent_key_produces_no_dedup_id() {
        assert!(DedupMasterKey(None).dedup_id(&[0u8; 32]).is_none());
        assert!(!DedupMasterKey(None).is_configured());
    }

    /// 密钥绝不进日志。
    #[test]
    fn the_key_never_renders_in_debug_output() {
        let rendered = format!("{:?}", key());
        assert!(rendered.contains("REDACTED"), "{rendered}");
        assert!(!rendered.contains('7'), "{rendered}");
        assert_eq!(format!("{:?}", DedupMasterKey(None)), "DedupMasterKey(absent)");
    }
}

#[cfg(test)]
mod attachment_config_tests {
    use super::ServerConfig;

    const KEY_A: &str = "oaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaE";
    const KEY_B: &str = "srKysrKysrKysrKysrKysrKysrKysrKysrKysrKysrI";

    /// 以仓库里的 `config.example.toml` 为基底：这样每条用例同时也在验证
    /// **模板本身仍然加载得起来**。模板腐化过一次就再也没人照它配了。
    fn load_with_attachment(section: &str) -> anyhow::Result<ServerConfig> {
        let base = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/config.example.toml"
        ))
        .expect("模板必须存在");
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, format!("{base}\n{section}\n")).expect("write");
        ServerConfig::from_toml_file(&path)
    }

    #[test]
    fn the_shipped_example_still_loads() {
        load_with_attachment("").expect("config.example.toml 必须能被自己的加载器读起来");
    }

    #[test]
    fn a_matching_pair_is_accepted() {
        let config = load_with_attachment(&format!(
            "[attachment]\ndedup_master_key = \"{KEY_A}\"\n\n[[attachment.keys]]\nid = 1\nkey = \"{KEY_B}\""
        ))
        .expect("成对配置应当被接受");
        assert!(config.dedup_master_key.is_configured());
        assert_eq!(config.attachment_keys.first().map(|(id, _)| *id), Some(1));
    }

    /// 🔴 只配加密密钥必须**拒绝启动**，不是静默关掉跨用户秒传。
    /// 缺了它运行期毫无迹象：用户只会看到每次发同一张图都在重新上传。
    #[test]
    fn encryption_without_a_dedup_key_refuses_to_start() {
        let err = load_with_attachment(&format!(
            "[[attachment.keys]]\nid = 1\nkey = \"{KEY_B}\""
        ))
        .expect_err("必须拒绝");
        assert!(format!("{err:#}").contains("dedup_master_key"), "{err:#}");
    }

    #[test]
    fn a_dedup_key_without_encryption_refuses_to_start() {
        let err = load_with_attachment(&format!(
            "[attachment]\ndedup_master_key = \"{KEY_A}\""
        ))
        .expect_err("必须拒绝");
        assert!(format!("{err:#}").contains("attachment.keys"), "{err:#}");
    }

    /// 🔴 两把相同等于"dedup 密钥从加密密钥派生"：加密密钥一轮换，运维很可能顺手
    /// 把这里也换掉，全站秒传索引当场作废。
    #[test]
    fn the_two_keys_must_not_be_the_same_material() {
        let err = load_with_attachment(&format!(
            "[attachment]\ndedup_master_key = \"{KEY_A}\"\n\n[[attachment.keys]]\nid = 1\nkey = \"{KEY_A}\""
        ))
        .expect_err("必须拒绝");
        assert!(format!("{err:#}").contains("不能与"), "{err:#}");
    }

    /// 相同密钥换一种写法（前后空格）仍然是同一把——所以比的是解码后的字节，
    /// 不是 base64 字符串。字符串比较会判为不同，而底下是同一把密钥。
    #[test]
    fn the_sameness_check_compares_decoded_bytes_not_the_encoding() {
        let err = load_with_attachment(&format!(
            "[attachment]\ndedup_master_key = \" {KEY_A} \"\n\n[[attachment.keys]]\nid = 1\nkey = \"{KEY_A}\""
        ))
        .expect_err("必须拒绝");
        assert!(format!("{err:#}").contains("不能与"), "{err:#}");
    }

    #[test]
    fn a_dedup_key_of_the_wrong_length_refuses_to_start() {
        let err = load_with_attachment(&format!(
            "[attachment]\ndedup_master_key = \"AAAA\"\n\n[[attachment.keys]]\nid = 1\nkey = \"{KEY_B}\""
        ))
        .expect_err("必须拒绝");
        assert!(format!("{err:#}").contains("32 字节"), "{err:#}");
    }
}

#[cfg(test)]
mod attachment_material_tests {
    use super::{
        attachment_material_from_toml, ServerConfig, TomlAttachmentConfig, TomlAttachmentKey,
        TomlConfig,
    };

    const KEY_A: &str = "oaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaGhoaE";
    const KEY_B: &str = "srKysrKysrKysrKysrKysrKysrKysrKysrKysrKysrI";
    const KEY_C: &str = "w8PDw8PDw8PDw8PDw8PDw8PDw8PDw8PDw8PDw8PDw8M";

    fn att(dedup: Option<&str>, keys: &[(u8, &str)]) -> TomlAttachmentConfig {
        TomlAttachmentConfig {
            dedup_master_key: dedup.map(str::to_string),
            keys: Some(
                keys.iter()
                    .map(|(id, key)| TomlAttachmentKey {
                        id: *id,
                        key: key.to_string(),
                    })
                    .collect(),
            ),
        }
    }

    fn err_of(dedup: Option<&str>, keys: &[(u8, &str)]) -> String {
        format!(
            "{:#}",
            attachment_material_from_toml(Some(&att(dedup, keys))).expect_err("必须拒绝")
        )
    }

    #[test]
    fn a_matching_pair_decodes() {
        let (keys, dedup) =
            attachment_material_from_toml(Some(&att(Some(KEY_A), &[(1, KEY_B)]))).expect("接受");
        assert!(dedup.is_configured());
        // 密钥表保留 base64 原文：它要原样下发给客户端。
        assert_eq!(keys.first(), Some(&(1u8, KEY_B.to_string())));
    }

    #[test]
    fn no_attachment_section_is_fine() {
        let (keys, dedup) = attachment_material_from_toml(None).expect("不配也行");
        assert!(keys.is_empty());
        assert!(!dedup.is_configured());
    }

    #[test]
    fn each_half_on_its_own_is_refused() {
        assert!(err_of(None, &[(1, KEY_B)]).contains("dedup_master_key"));
        assert!(err_of(Some(KEY_A), &[]).contains("attachment.keys"));
    }

    #[test]
    fn the_two_keys_must_differ() {
        assert!(err_of(Some(KEY_A), &[(1, KEY_A)]).contains("不能与"));
        // 写法不同、密钥相同也要拦住：比的是解码后的字节。
        assert!(err_of(Some(&format!(" {KEY_A} ")), &[(1, KEY_A)]).contains("不能与"));
    }

    #[test]
    fn duplicate_key_ids_and_duplicate_material_are_refused() {
        assert!(err_of(Some(KEY_A), &[(1, KEY_B), (1, KEY_C)]).contains("key_id 重复"));
        assert!(err_of(Some(KEY_A), &[(1, KEY_B), (2, KEY_B)]).contains("重复的密钥内容"));
    }

    #[test]
    fn malformed_material_is_refused() {
        assert!(err_of(Some("AAAA"), &[(1, KEY_B)]).contains("32 字节"));
        assert!(err_of(Some(KEY_A), &[(1, "!!!!")]).contains("base64url"));
    }

    /// 🔴 这条才是重构的意义：**结构体转换路径**同样执行全部规则。
    ///
    /// 之前 `From<TomlConfig>` 只在编码错误时 panic，成对性、密钥相同、id 重复
    /// 一条都不查——从这条路径构造出来的配置可以带着一半的附件密钥启动。
    #[test]
    fn the_struct_conversion_path_enforces_the_same_rules() {
        let toml: TomlConfig = toml::from_str(&format!(
            "[attachment]\n\n[[attachment.keys]]\nid = 1\nkey = \"{KEY_B}\"\n"
        ))
        .expect("parse");
        let err = ServerConfig::try_from(toml).expect_err("缺 dedup 密钥必须拒绝");
        assert!(format!("{err:#}").contains("dedup_master_key"), "{err:#}");
    }
}
