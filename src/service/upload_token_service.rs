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

//! 上传 Token 服务
//!
//! 管理临时上传 token，用于文件上传的权限控制

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::error::{Result, ServerError};
use crate::service::file_service::FileType;

/// 这张 token 被授权做什么——**冻结的能力边界**，不是流程阶段。
///
/// 🔴 token 现在可复用（24 小时内反复用于上传 / 查状态 / complete），
/// **可复用不等于可跨入口**：预检命中签的是 `ClaimExisting`，只能去换自己的
/// `file_id`；未命中签的是 `Upload`，只能去传字节。用途签进 token，两个入口
/// 各自拒绝不属于自己的那种，一张 token 因此不可能同时留下一条 claim 行和
/// 一条上传行。
///
/// 📌 早先这条边界靠「一次性消费」兜着，那个机制已经删除（见 [`UploadToken`]），
/// 现在完全由签进去的用途承担。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadTokenPurpose {
    /// 预检未命中：这张 token 只能拿去传字节。
    Upload,
    /// 预检命中：这张 token 只能拿去换自己的 file_id，不能再传字节。
    ClaimExisting,
}

impl Default for UploadTokenPurpose {
    /// 老 token（Redis 里没有这个字段）按实体上传处理，兼容滚动升级。
    fn default() -> Self {
        Self::Upload
    }
}

/// prepare 阶段声明的文件身份，签进 token 后在完成时逐项复核。
#[derive(Debug, Clone, Default)]
pub struct UploadIdentity {
    pub sha256: Option<String>,
    pub declared_size: Option<i64>,
    pub mime_type: Option<String>,
    pub transform_version: i32,
}

/// 上传 Token 信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadToken {
    /// Token（UUID）
    pub token: String,
    /// 发起上传的用户 ID
    pub user_id: u64,
    /// 文件类型
    pub file_type: FileType,
    /// 允许的最大文件大小（字节）
    pub max_size: i64,
    /// 业务类型（avatar/message/group_file/...）
    pub business_type: String,
    /// 原始文件名（可选）
    pub filename: Option<String>,
    /// 客户端在 prepare 阶段声明的最终内容摘要（SHA-256 十六进制）。
    ///
    /// 🔴 文件身份必须**绑在 token 里**，完成时逐项复核。只信请求参数的话，
    /// 客户端可以在 prepare 与 upload 之间换掉摘要、大小或处理版本——
    /// 那样秒传判定用的是一组参数，落库用的是另一组。
    #[serde(default)]
    pub sha256: Option<String>,
    /// 声明的精确大小（字节）。`max_size` 是上限，这是「就该是这么大」。
    #[serde(default)]
    pub declared_size: Option<i64>,
    /// 声明的 MIME。
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 产出这份字节的客户端处理版本。
    #[serde(default)]
    pub transform_version: i32,
    /// 这张 token 的用途，见 [`UploadTokenPurpose`]。
    #[serde(default)]
    pub purpose: UploadTokenPurpose,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 过期时间。由签发方按配置 TTL 给出（缺省 24 小时），见 [`UploadToken::new`]。
    pub expires_at: DateTime<Utc>,
    /// 🔴 **历史字段，只为反序列化迁移期还留在 Redis 里的旧记录。**
    ///
    /// 一次性消费语义已取消：一张 token 在 24 小时内要被分片、查状态、complete
    /// 反复使用。新代码**不得**再置位它——设成 true 就等于把那次断点续传永久烧掉。
    /// 销毁 token 的 API（`mark_token_used` / `remove_token`）已连同调用点一并删除。
    #[serde(default)]
    pub used: bool,
}

impl UploadToken {
    /// 创建新的上传 token
    pub fn new(
        user_id: u64,
        file_type: FileType,
        max_size: i64,
        business_type: String,
        filename: Option<String>,
        identity: UploadIdentity,
        purpose: UploadTokenPurpose,
        ttl_secs: i64,
    ) -> Self {
        let now = Utc::now();
        let token = Uuid::new_v4().to_string();

        Self {
            token,
            user_id,
            file_type,
            max_size,
            business_type,
            filename,
            sha256: identity.sha256,
            declared_size: identity.declared_size,
            mime_type: identity.mime_type,
            transform_version: identity.transform_version,
            purpose,
            created_at: now,
            // 🔴 有效期由调用方给，**新旧 token 一个口径**（产品拍板：一种 token、
            // 24 小时）。格式可以不同，签发语义不能不同——否则响应说 24 小时、
            // 报一个与实际不符的有效期，客户端会持久化一个早已作废的凭证。
            expires_at: now + Duration::seconds(ttl_secs),
            used: false,
        }
    }

    /// 检查 token 是否有效
    pub fn is_valid(&self) -> bool {
        !self.used && Utc::now() < self.expires_at
    }

}

/// 日志用 token 脱敏：只保留前 8 位（P0-10：上传 token 不落明文日志）。
fn redact(token: &str) -> String {
    format!("{}…", token.chars().take(8).collect::<String>())
}

/// 派生 `upload_id` 时的域分隔前缀。
///
/// 加前缀是为了让这个哈希只在「旧 token → upload_id」这一个用途下有意义：
/// 不加的话，任何别处对同一字符串取 SHA-256 的地方都会算出同一个值。
const LEGACY_UPLOAD_ID_DOMAIN: &[u8] = b"legacy-upload-id\0";

/// 旧 UUID token 没有 `upload_id`，但会话目录、模式锁和完成幂等（预留 `file_id`
/// + 墓碑）全都以它为轴。这里从 token 稳定派生一个。
///
/// 🔴 **不能直接拿 token 当 `upload_id`**：它是 bearer 凭证，而 `upload_id` 会成为
/// 目录名和日志字段（RESUMABLE_UPLOAD_SPEC §5.2.3 / §7）。
///
/// 纯函数 ⇒ 同一张旧 token 的每次重试都落到同一个会话目录，模式锁与完成幂等照常生效。
pub fn derive_legacy_upload_id(token: &str) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(LEGACY_UPLOAD_ID_DOMAIN);
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

/// 服务端下发的上传方案（RESUMABLE_UPLOAD_SPEC §3.1）。
///
/// **缺席即整包直传**——这既是旧客户端的兼容路径，也是分片链路的关停阀：
/// 服务端停发这一段，所有新客户端当场退回整包，不必等三端发版。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadPlan {
    /// 区间寻址网格；冻结。客户端每次请求传几个 unit 由它自己按实测吞吐决定。
    pub base_unit: u32,
    /// 首个探测请求的大小。
    pub initial_request_size: u32,
    /// 单次请求上限。
    pub max_request_size: u32,
    /// 小于等于此值不值得建会话，直接整包传。
    pub session_threshold: u64,
    /// 并发上限。
    pub max_parallel_parts: u8,
}

/// 上传凭证验证后的统一结果。
///
/// 迁移期同时存在两种 token 格式（自包含签名 token 与旧的 Redis UUID token）。
/// 验证器只输出这一个模型，**调用方不再判断 token 是什么格式**——否则模式锁、
/// 会话目录、完成幂等每一处都要各写一遍分叉。
///
/// 🔴 **过期判定不在这里。** 有效期、时钟宽限和「以请求开始时刻为准」由验证器一次性
/// 处理；拿到本模型即表示已经验过。模型自己再查一次当前时间，就是第二套判定，
/// 迟早与验证器的口径分家。`expires_at` 只作数据保留，供日志与墓碑保留期使用。
#[derive(Debug, Clone)]
pub struct ValidatedUploadToken {
    /// 这次上传的唯一标识：会话目录名、模式锁与完成幂等（`reserved_file_id` + 墓碑）的轴。
    ///
    /// 签名 token 直接读签进去的值；旧 UUID token 走 [`derive_legacy_upload_id`]。
    pub upload_id: String,
    pub user_id: u64,
    pub purpose: UploadTokenPurpose,
    pub file_type: FileType,
    pub max_size: i64,
    pub business_type: String,
    pub filename: Option<String>,
    /// prepare 阶段冻结的文件身份，完成时逐项复核。
    pub sha256: Option<String>,
    /// 最终要上传的**密文**字节数。
    ///
    /// 📌 旧 token 的 `declared_size` 就是这个量（`file_service` 拿它与实际落盘字节数
    /// 比对），换名不换义，映射时不需要任何换算。
    pub sealed_blob_size: Option<i64>,
    pub mime_type: Option<String>,
    pub transform_version: i32,
    /// 附件加密版本。旧 token 不带（当时由 multipart 表单提供）→ `None`。
    pub encryption_version: Option<i32>,
    /// 服务端下发的上传方案；`None` = 整包直传（旧 token 恒为 `None`）。
    pub upload_plan: Option<UploadPlan>,
    /// 分片字节落在哪个节点。`None` = 本节点（旧 token 没有这个概念）。
    pub node_id: Option<String>,
    /// 后续请求应当发往的地址。`None` = 本节点。
    pub upload_base_url: Option<String>,
    /// 仅作数据保留；**不要拿它再判一次过期**（见上）。
    pub expires_at: DateTime<Utc>,
}

impl ValidatedUploadToken {
    /// 这条**正式文件记录**是不是这张 token 描述的那份上传。
    ///
    /// 🔴 临时会话状态（`state.json` 的墓碑 / `reserved_file_id`）只能用来**定位**
    /// 候选 `file_id`，不能单独构成「可以把这条正式记录交给你」的授权：同一个用户
    /// 名下有成千上万个附件，只比 uploader 等于把别的文件当成本次上传的结果返回。
    ///
    /// 判据取 token 里**冻结**的事实，所有幂等出口（HTTP 墓碑返回、预留恢复、
    /// 完成回调）必须共用这一个，不许各写一份。
    pub fn matches_file(&self, meta: &crate::service::file_service::FileMetadata) -> bool {
        if meta.uploader_id != self.user_id {
            return false;
        }
        if meta.file_type.as_str() != self.file_type.as_str() {
            return false;
        }
        if let Some(sha) = self.sha256.as_deref() {
            // 🔴 摘要比较不区分大小写：客户端报大写十六进制是合法的，而服务端
            // 算出来的恒为小写。用精确比较会让「首次成功、重试报身份不符」。
            match meta.file_hash.as_deref() {
                Some(stored) if stored.eq_ignore_ascii_case(sha) => {}
                _ => return false,
            }
        }
        if let Some(size) = self.sealed_blob_size {
            if meta.file_size as i64 != size {
                return false;
            }
        }
        true
    }

    /// 旧 Redis UUID token 的投影。
    ///
    /// 🔴 `raw_token` 必须是**客户端这次提交的原始凭证**，不能用 `record.token`：
    /// 后者是 Redis 里序列化记录中的字段，正常情况下两者相同，但一旦分家
    /// （写入与 key 不一致、记录被改写），`upload_id` 就会落到另一个目录，
    /// 于是模式锁、会话目录和完成幂等三者集体指错地方。派生只认收到的那一份。
    /// 自包含签名 token 的投影：所有字段都是签进去的，无需派生。
    pub fn from_claims(c: &crate::security::upload_token::UploadTokenClaims) -> Self {
        Self {
            upload_id: c.upload_id.clone(),
            user_id: c.uid,
            // verify() 已经拒过未知用途，这里的 unwrap_or 只是不 panic 的写法。
            purpose: c.purpose().unwrap_or(UploadTokenPurpose::Upload),
            file_type: crate::model::file_upload::FileType::from_str(&c.ft)
                .unwrap_or(crate::model::file_upload::FileType::File),
            max_size: c.mx,
            business_type: c.bt.clone(),
            filename: c.filename.clone(),
            sha256: c.sha256.clone(),
            sealed_blob_size: c.sealed_blob_size,
            mime_type: c.mime_type.clone(),
            transform_version: c.tv,
            encryption_version: c.encryption_version,
            upload_plan: c.upload_plan(),
            node_id: c.node_id.clone(),
            upload_base_url: c.upload_base_url.clone(),
            expires_at: DateTime::from_timestamp(c.exp as i64, 0).unwrap_or_else(Utc::now),
        }
    }

    pub fn from_legacy(raw_token: &str, record: &UploadToken) -> Self {
        Self {
            upload_id: derive_legacy_upload_id(raw_token),
            user_id: record.user_id,
            purpose: record.purpose,
            file_type: record.file_type.clone(),
            max_size: record.max_size,
            business_type: record.business_type.clone(),
            filename: record.filename.clone(),
            sha256: record.sha256.clone(),
            sealed_blob_size: record.declared_size,
            mime_type: record.mime_type.clone(),
            transform_version: record.transform_version,
            // 下面四项旧 token 提供不了：加密版本当时走 multipart 表单，
            // 分片方案与节点绑定是新协议才有的概念。
            encryption_version: None,
            upload_plan: None,
            node_id: None,
            upload_base_url: None,
            expires_at: record.expires_at,
        }
    }
}

/// 上传 Token 服务
///
/// Redis（key `upload_token:{token}`，SETEX 到期自灭）；无 Redis 时回退进程内存
/// （单实例/测试）。
///
/// 📌 **不再有一次性消费。** 早先用 GETDEL 保证「同一张 token 只能用一次」，
/// 而现在一张 token 在 24 小时内要被反复使用（分片、查状态、complete）。
/// 重放由完成幂等（预留 `file_id` + 主键）与模式锁承担，不靠烧 token。
pub struct UploadTokenService {
    /// 内存回退存储（token -> UploadToken）
    tokens: Arc<RwLock<HashMap<String, UploadToken>>>,
    redis: Option<Arc<crate::infra::redis::RedisClient>>,
    /// 签名 token 配置。`None` = 未配置密钥，只剩旧 UUID 路径（与今天行为一致）。
    signing: Option<crate::security::upload_token::UploadTokenConfig>,
}

/// 一串上传凭证长什么样。
///
/// 🔴 **必须在验证之前分类，而且三类互斥。** 迁移期两种格式并存，如果「签名验失败」
/// 会掉进旧 Redis 查询分支，那就等于给攻击者一条**降级**通道：伪造一个 kid 错、
/// 签名错的 JWT，让服务端去查 Redis。分类之后，三段点分的串**只走签名验证，失败即拒**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialShape {
    /// 三段点分：只按签名 token 验，失败绝不回退。
    Signed,
    /// 合法 UUID：走旧 Redis 路径。
    LegacyUuid,
    /// 其它一律直接拒绝——不去查任何后端。
    Unrecognised,
}

/// 判形。只看形状，不做任何验证。
pub fn classify_credential(token: &str) -> CredentialShape {
    if token.split('.').count() == 3 {
        return CredentialShape::Signed;
    }
    if Uuid::parse_str(token).is_ok() {
        return CredentialShape::LegacyUuid;
    }
    CredentialShape::Unrecognised
}

const REDIS_KEY_PREFIX: &str = "upload_token:";

impl UploadTokenService {
    /// 创建新的 UploadTokenService（内存模式，测试/无 Redis 场景）
    pub fn new() -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            redis: None,
            signing: None,
        }
    }

    /// 挂上签名 token 配置（`[upload.token]`）。
    pub fn with_signing(
        mut self,
        cfg: Option<crate::security::upload_token::UploadTokenConfig>,
    ) -> Self {
        self.signing = cfg;
        self
    }

    /// 这次上传该不该分片：返回 `Some(plan)` = 分片，`None` = 整包直传。
    ///
    /// 🔴 判断只在服务端一处。客户端**不实现「多大算小文件」**——收到 plan 就分片，
    /// 没收到就整包。阈值调整因此不需要三端发版；恒不下发即是关停阀。
    ///
    /// 目前只在签发签名 token 时给出方案：旧 UUID token 没有承载它的字段。
    pub fn plan_for(&self, sealed_blob_size: i64) -> Option<UploadPlan> {
        if !self.issues_signed() {
            return None;
        }
        const BASE_UNIT: u32 = 64 * 1024;
        let plan = UploadPlan {
            base_unit: BASE_UNIT,
            initial_request_size: BASE_UNIT,
            max_request_size: 2 * 1024 * 1024,
            session_threshold: BASE_UNIT as u64,
            // 🔴 **1，不是 3。**
            //
            // 每个分片请求都要抢整个会话的非阻塞排他锁，所以第二个并发请求会立刻
            // 拿到「忙」。下发 3 等于让客户端去撞一堵墙，还把「忙」当成服务端不稳定。
            // 真要并发，得先把写入做成区间级、只串行化 journal 与状态切换——那是
            // 独立一件事；在那之前，**下发的数字必须等于服务端真能做到的数字**。
            max_parallel_parts: 1,
        };
        if sealed_blob_size <= plan.session_threshold as i64 {
            return None;
        }
        Some(plan)
    }

    /// 当前是否按签名格式签发。未配密钥时恒为 false。
    pub fn issues_signed(&self) -> bool {
        matches!(
            self.signing.as_ref().map(|c| c.issue_mode),
            Some(crate::security::upload_token::IssueMode::Signed)
        )
    }

    /// 带 Redis 后端创建（生产路径）：token 状态跨实例可见。
    pub fn new_with_redis(redis: Arc<crate::infra::redis::RedisClient>) -> Self {
        Self {
            tokens: Arc::new(RwLock::new(HashMap::new())),
            redis: Some(redis),
            signing: None,
        }
    }

    /// 签发有效期（秒）。未配 `[upload.token]` 时用统一默认值 24 小时。
    fn ttl_secs(&self) -> i64 {
        self.signing
            .as_ref()
            .map(|c| c.ttl_secs as i64)
            .unwrap_or(crate::security::upload_token::MAX_TTL_SECS as i64)
    }

    /// 生成上传 token
    pub async fn generate_token(
        &self,
        user_id: u64,
        file_type: FileType,
        max_size: i64,
        business_type: String,
        filename: Option<String>,
        identity: UploadIdentity,
        purpose: UploadTokenPurpose,
    ) -> Result<UploadToken> {
        let token = UploadToken::new(
            user_id,
            file_type,
            max_size,
            business_type,
            filename,
            identity,
            purpose,
            self.ttl_secs(),
        );

        if let Some(redis) = &self.redis {
            let ttl = (token.expires_at - Utc::now()).num_seconds().max(1) as usize;
            let payload = serde_json::to_string(&token)
                .map_err(|e| ServerError::Internal(format!("序列化 upload token 失败: {}", e)))?;
            redis
                .setex(&format!("{REDIS_KEY_PREFIX}{}", token.token), ttl, &payload)
                .await?;
        } else {
            self.tokens
                .write()
                .await
                .insert(token.token.clone(), token.clone());
        }

        info!(
            "🎫 生成上传 token: {} (用户: {}, 类型: {}, 最大: {} bytes, 业务: {})",
            redact(&token.token),
            token.user_id,
            token.file_type.as_str(),
            token.max_size,
            token.business_type
        );

        Ok(token)
    }

    /// 🔴 **唯一的验证入口**：按形状分类后各走各的，输出统一模型。
    ///
    /// - 三段点分 → 只按签名验证，失败**直接拒绝**，绝不回退 Redis
    /// - 合法 UUID → 旧 Redis / 内存路径
    /// - 其它 → 直接拒绝，不查任何后端
    ///
    /// `now_secs` 是**请求开始时刻**（spec §5.3）：一个开始时有效的长传输不因为
    /// 传输途中跨过过期时刻而失败。
    pub async fn validate_any(&self, now_secs: u64, token: &str) -> Result<ValidatedUploadToken> {
        match classify_credential(token) {
            CredentialShape::Signed => {
                let cfg = self.signing.as_ref().ok_or_else(|| {
                    warn!("❌ 收到签名 token 但未配置 [upload.token]: {}", redact(token));
                    ServerError::InvalidToken
                })?;
                let claims = crate::security::upload_token::verify(cfg, now_secs, token)
                    .map_err(|e| {
                        warn!("❌ 签名 token 验证失败({}): {}", e, redact(token));
                        ServerError::InvalidToken
                    })?;
                Ok(ValidatedUploadToken::from_claims(&claims))
            }
            CredentialShape::LegacyUuid => {
                let record = self.validate_token(token).await?;
                Ok(ValidatedUploadToken::from_legacy(token, &record))
            }
            CredentialShape::Unrecognised => {
                warn!("❌ 无法识别的上传凭证格式: {}", redact(token));
                Err(ServerError::InvalidToken)
            }
        }
    }

    /// 按当前 `issue_mode` 签发。返回 `(token 字符串, upload_id, 真实过期时刻)`。
    ///
    /// 🔴 **`issue_mode` 只回滚「签成什么格式」，不回滚语义。**
    ///
    /// `legacy_uuid` 签的仍然是 **24 小时、可复用**的 token，一次性消费已经删除，
    /// 整包路径同样受会话模式锁与完成幂等约束。也就是说它**不等于「回到改动前」**：
    /// 它的用途是「签名 token 出了问题时退回旧格式」，验证侧继续双验。
    ///
    /// 真要回滚到旧语义，只能回滚版本。
    ///
    /// 🔴 **过期时刻必须由签发路径给出，不能由调用方按「反正是 24h」推算。**
    /// 两条分支现在都用配置 TTL（缺省 24 小时），但「推算」和「给出」仍是两回事：
    /// 将来任何一条分支改了有效期，响应必须跟着变，而不是继续报一个想当然的值。
    ///
    /// `issue_mode = legacy_uuid`（缺省）签出的是**旧格式**（Redis UUID），
    /// 旧客户端照常透明使用；有效期与可复用语义**不随之回滚**（见上）。
    #[allow(clippy::too_many_arguments)]
    pub async fn issue(
        &self,
        now_secs: u64,
        user_id: u64,
        file_type: FileType,
        max_size: i64,
        business_type: String,
        filename: Option<String>,
        identity: UploadIdentity,
        purpose: UploadTokenPurpose,
        upload_plan: Option<&UploadPlan>,
    ) -> Result<(String, String, i64)> {
        // 🔴 **摘要规范化只在这里做一次**，覆盖所有签发路径。
        //
        // 放在调用方（某个 RPC）里的话，将来多一条签发入口就会漏掉：客户端报大写
        // 十六进制是合法的，而服务端算出来的恒为小写，签了大写就会「首次上传成功、
        // 重试报身份不符」。
        let identity = UploadIdentity {
            sha256: identity
                .sha256
                .map(|d| d.trim().to_ascii_lowercase()),
            ..identity
        };
        if self.issues_signed() {
            let cfg = self
                .signing
                .as_ref()
                .expect("issues_signed() 已保证配置存在");
            // upload_id 由服务端生成：128 位随机，十六进制（目录名安全）。
            let upload_id = Uuid::new_v4().simple().to_string();
            let mut claims = crate::security::upload_token::UploadTokenClaims::new(
                upload_id.clone(),
                user_id,
                purpose,
                file_type.as_str(),
                business_type,
                max_size,
                identity.transform_version,
            );
            claims.filename = filename;
            claims.sha256 = identity.sha256;
            claims.sealed_blob_size = identity.declared_size;
            claims.mime_type = identity.mime_type;
            claims.set_upload_plan(upload_plan);
            let token = crate::security::upload_token::sign(cfg, now_secs, claims)
                .map_err(|e| ServerError::Validation(format!("签发上传 token 失败: {e}")))?;
            info!(
                "🎫 签发上传 token(signed): upload_id={} 用户={} 类型={} 业务已签入",
                upload_id,
                user_id,
                file_type.as_str()
            );
            return Ok((token, upload_id, now_secs as i64 + cfg.ttl_secs as i64));
        }

        let record = self
            .generate_token(
                user_id,
                file_type,
                max_size,
                business_type,
                filename,
                identity,
                purpose,
            )
            .await?;
        let upload_id = derive_legacy_upload_id(&record.token);
        let expires_at = record.expires_at.timestamp();
        Ok((record.token, upload_id, expires_at))
    }

    /// 验证**旧 UUID token**（Redis / 内存）。
    ///
    /// 🔴 **私有**：迁移期只要有一个调用点直接用它，那条路径就只认旧格式——
    /// `issue_mode` 一切到 signed 就当场断掉。秒传入口正是这么断过一次。
    /// 外部一律走 [`Self::validate_any`]，让「漏迁一个点」在编译期就不可能。
    async fn validate_token(&self, token: &str) -> Result<UploadToken> {
        if let Some(redis) = &self.redis {
            let payload = redis.get(&format!("{REDIS_KEY_PREFIX}{token}")).await?;
            return match payload.and_then(|p| serde_json::from_str::<UploadToken>(&p).ok()) {
                Some(upload_token) if upload_token.is_valid() => {
                    debug!("✅ Token 验证通过: {}", redact(token));
                    Ok(upload_token)
                }
                _ => {
                    warn!("❌ Token 不存在或已失效: {}", redact(token));
                    Err(ServerError::InvalidToken)
                }
            };
        }

        let tokens = self.tokens.read().await;
        match tokens.get(token) {
            Some(upload_token) => {
                if upload_token.is_valid() {
                    debug!("✅ Token 验证通过: {}", redact(token));
                    Ok(upload_token.clone())
                } else {
                    warn!(
                        "❌ Token 已失效: {} (已使用: {}, 过期: {})",
                        redact(token),
                        upload_token.used,
                        Utc::now() >= upload_token.expires_at
                    );
                    Err(ServerError::InvalidToken)
                }
            }
            None => {
                warn!("❌ Token 不存在: {}", redact(token));
                Err(ServerError::InvalidToken)
            }
        }
    }

    /// 清理过期的 token（定期调用）。Redis 路径由 SETEX TTL 自灭，无需清理。
    pub async fn cleanup_expired_tokens(&self) {
        if self.redis.is_some() {
            return;
        }
        let mut tokens = self.tokens.write().await;
        let now = Utc::now();

        let expired_tokens: Vec<String> = tokens
            .iter()
            .filter(|(_, token)| token.expires_at < now)
            .map(|(key, _)| key.clone())
            .collect();

        for token in &expired_tokens {
            tokens.remove(token);
        }

        if !expired_tokens.is_empty() {
            info!("🧹 清理过期 token: {} 个", expired_tokens.len());
        }
    }

    /// 获取当前 token 数量（用于监控）
    pub async fn token_count(&self) -> usize {
        self.tokens.read().await.len()
    }
}

impl Default for UploadTokenService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_generate_and_validate_token() {
        let service = UploadTokenService::new();

        // 生成 token
        let token = service
            .generate_token(
                1001,
                FileType::Image,
                10485760, // 10MB
                "message".to_string(),
                Some("test.jpg".to_string()),
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
            )
            .await
            .unwrap();

        // 验证 token
        let validated = service.validate_token(&token.token).await.unwrap();
        assert_eq!(validated.user_id, 1001);
        assert_eq!(validated.file_type, FileType::Image);
        assert_eq!(validated.business_type, "message");
        assert!(validated.is_valid());
    }

    /// 🔴 门禁：**claim 用途的 signed token 必须验得过统一入口**。
    ///
    /// `claim_existing_file` 曾经只调 `validate_token`（仅认 Redis UUID）。
    /// 那样一旦 `issue_mode` 切到 signed，预检命中签出的 signed claim token
    /// 在秒传入口当场被判无效——**秒传全线断掉**，而且是在 callback 之前就断。
    /// 迁移期漏掉一个验证点，效果和没迁一样。
    #[tokio::test]
    async fn a_signed_claim_token_passes_the_unified_validator() {
        let mut keys = std::collections::HashMap::new();
        keys.insert("upload-v1".to_string(), "s3cr3t".to_string());
        let service = UploadTokenService::new().with_signing(Some(
            crate::security::upload_token::UploadTokenConfig {
                keys,
                default_kid: "upload-v1".to_string(),
                leeway_secs: 30,
                ttl_secs: crate::security::upload_token::MAX_TTL_SECS,
                issue_mode: crate::security::upload_token::IssueMode::Signed,
            },
        ));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // 预检命中 → 签 claim 用途的 signed token。
        let (token, upload_id, _exp) = service
            .issue(
                now,
                1001,
                FileType::Image,
                10485760,
                "message".to_string(),
                None,
                UploadIdentity {
                    sha256: Some("a".repeat(64)),
                    declared_size: Some(4096),
                    mime_type: Some("image/png".to_string()),
                    transform_version: 0,
                },
                UploadTokenPurpose::ClaimExisting,
                None,
            )
            .await
            .expect("issue");
        assert_eq!(
            classify_credential(&token),
            CredentialShape::Signed,
            "signed 模式必须签出签名 token"
        );

        // 秒传入口用的就是这个统一入口。
        let validated = service
            .validate_any(now, &token)
            .await
            .expect("signed claim token 必须验得过");
        assert_eq!(validated.purpose, UploadTokenPurpose::ClaimExisting);
        assert_eq!(validated.upload_id, upload_id);
        assert_eq!(validated.user_id, 1001);
    }

    /// 🔴 门禁：**签发路径**必须把摘要规范化后再签进去。
    ///
    /// 只测 `matches_file` 的大小写容忍是不够的——那只证明比较处兜得住，
    /// 证明不了 token 里存的是小写。删掉 `issue()` 里的规范化，这条必须变红。
    #[tokio::test]
    async fn issuing_normalises_the_digest_before_signing_it() {
        let service = UploadTokenService::new();
        let (token, _upload_id, _exp) = service
            .issue(
                1_700_000_000,
                1001,
                FileType::Image,
                10485760,
                "message".to_string(),
                None,
                UploadIdentity {
                    sha256: Some("A".repeat(64)), // 客户端报大写
                    declared_size: Some(4096),
                    mime_type: Some("image/png".to_string()),
                    transform_version: 0,
                },
                UploadTokenPurpose::Upload,
                None,
            )
            .await
            .expect("issue");

        let validated = service
            .validate_any(1_700_000_000, &token)
            .await
            .expect("validate");
        assert_eq!(
            validated.sha256.as_deref(),
            Some("a".repeat(64).as_str()),
            "签进 token 的摘要必须已规范化为小写"
        );
    }

    /// 🔴 客户端报大写十六进制摘要是合法的，而服务端算出来的恒为小写。
    /// 精确比较会让「首次上传成功、重试报身份不符」——一次合法重试被判成攻击。
    #[test]
    fn a_digest_in_upper_case_still_matches_the_stored_one() {
        use crate::service::file_service::FileMetadata;
        let mut token = ValidatedUploadToken::from_legacy(
            "b3f1a2c4-0000-4000-8000-000000000001",
            &UploadToken::new(
                42,
                FileType::Image,
                1024,
                "message".to_string(),
                None,
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
                600,
            ),
        );
        token.sha256 = Some("A".repeat(64)); // 客户端报大写
        token.sealed_blob_size = Some(4096);

        let meta = FileMetadata {
            file_id: 1,
            original_filename: "x.png".to_string(),
            file_size: 4096,
            original_size: None,
            file_type: FileType::Image,
            mime_type: "image/png".to_string(),
            file_path: "images/1.png".to_string(),
            storage_source_id: 0,
            uploader_id: 42,
            uploader_ip: None,
            uploaded_at: 0,
            width: None,
            height: None,
            file_hash: Some("a".repeat(64)), // 服务端算出来的小写
            business_type: Some("message".to_string()),
            business_id: None,
            encryption_version: 0,
            cek: None,
        };
        assert!(token.matches_file(&meta), "大小写不同的同一个摘要必须视为相同");

        // 真正不同的摘要仍然要拒。
        let mut other = meta.clone();
        other.file_hash = Some("b".repeat(64));
        assert!(!token.matches_file(&other));

        // 同一用户的**另一个**附件（大小不同）也要拒——只比 uploader 是不够的。
        let mut different_file = meta.clone();
        different_file.file_size = 8192;
        assert!(!token.matches_file(&different_file));
    }

    /// 🔴 产品口径是「一种 token，24 小时」。格式可以不同，**签发语义不能不同**——
    /// legacy 分支若还签 5 分钟，响应说 24 小时就是在骗客户端。
    #[tokio::test]
    async fn a_legacy_token_gets_the_same_lifetime_as_a_signed_one() {
        let service = UploadTokenService::new();
        let now = chrono::Utc::now();
        let token = service
            .generate_token(
                1001,
                FileType::Image,
                10485760,
                "message".to_string(),
                None,
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
            )
            .await
            .unwrap();

        let lifetime = (token.expires_at - now).num_seconds();
        assert!(
            (86_400 - 60..=86_400 + 60).contains(&lifetime),
            "legacy token 有效期应为 24 小时，实际 {lifetime} 秒"
        );
    }

    #[tokio::test]
    async fn test_invalid_token() {
        let service = UploadTokenService::new();

        let result = service.validate_token("invalid-token").await;
        assert!(result.is_err());
    }

    /// 会话目录、模式锁和完成幂等都按 `upload_id` 定位。旧 token 的重试必须
    /// 落回同一个目录，否则每次重试都会开一个新会话，断点续传无从谈起。
    #[test]
    fn the_same_legacy_token_always_names_the_same_upload() {
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        assert_eq!(derive_legacy_upload_id(token), derive_legacy_upload_id(token));
    }

    /// 名字只承诺样例：两个样例证不了「任意两张 token 都不撞」，那是 SHA-256 的
    /// 抗碰撞性，不是这条测试能给的保证。
    #[test]
    fn different_legacy_tokens_derive_different_sample_ids() {
        let a = derive_legacy_upload_id("b3f1a2c4-0000-4000-8000-000000000001");
        let b = derive_legacy_upload_id("b3f1a2c4-0000-4000-8000-000000000002");
        assert_ne!(a, b);
    }

    /// `upload_id` 会成为磁盘上的目录名和日志字段；token 是 bearer 凭证。
    /// 派生值一旦等于（或包含）token 本身，凭证就随着目录列表泄露了。
    #[test]
    fn the_upload_id_never_leaks_the_bearer_token() {
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        let id = derive_legacy_upload_id(token);
        assert_ne!(id, token);
        assert!(!id.contains(token));
        // 目录名安全：只能是十六进制。
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(id.len(), 64);
    }

    /// 域分隔的意义：别处对同一字符串取裸 SHA-256 时，不会算出同一个 upload_id。
    #[test]
    fn the_derivation_is_domain_separated_from_a_bare_digest() {
        use sha2::Digest as _;
        let token = "b3f1a2c4-0000-4000-8000-000000000001";
        let bare = hex::encode(sha2::Sha256::digest(token.as_bytes()));
        assert_ne!(derive_legacy_upload_id(token), bare);
    }

    /// 统一模型必须把冻结的文件身份原样带过：complete 时要拿它逐项复核，
    /// 漏一个字段就等于那一项没有被校验。
    #[tokio::test]
    async fn the_validated_projection_carries_the_frozen_identity() {
        let service = UploadTokenService::new();
        let identity = UploadIdentity {
            sha256: Some("a".repeat(64)),
            declared_size: Some(4096),
            mime_type: Some("image/png".to_string()),
            transform_version: 7,
        };
        let token = service
            .generate_token(
                1001,
                FileType::Image,
                10485760,
                "message".to_string(),
                Some("holiday.png".to_string()),
                identity,
                UploadTokenPurpose::ClaimExisting,
            )
            .await
            .unwrap();

        let validated = ValidatedUploadToken::from_legacy(&token.token, &token);

        assert_eq!(validated.upload_id, derive_legacy_upload_id(&token.token));
        assert_eq!(validated.user_id, 1001);
        assert_eq!(validated.purpose, UploadTokenPurpose::ClaimExisting);
        assert_eq!(validated.business_type, "message");
        assert_eq!(validated.filename.as_deref(), Some("holiday.png"));
        assert_eq!(validated.sha256, Some("a".repeat(64)));
        // 换名不换义：老字段本来就是密文字节数（file_service 拿它与落盘字节数比对）。
        assert_eq!(validated.sealed_blob_size, Some(4096));
        assert_eq!(validated.mime_type.as_deref(), Some("image/png"));
        assert_eq!(validated.transform_version, 7);
        assert_eq!(validated.max_size, 10485760);
        assert_eq!(validated.expires_at, token.expires_at);
    }

    /// 旧 token 给不出的四项必须是明确的「没有」，不能靠调用点各自脑补默认值。
    #[tokio::test]
    async fn a_legacy_token_declares_the_new_fields_absent() {
        let service = UploadTokenService::new();
        let token = service
            .generate_token(
                7,
                FileType::File,
                1024,
                "message".to_string(),
                None,
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
            )
            .await
            .unwrap();

        let validated = ValidatedUploadToken::from_legacy(&token.token, &token);

        // upload_plan 缺席 = 整包直传，正是旧客户端唯一会走的路。
        assert!(validated.upload_plan.is_none());
        // 加密版本当时走 multipart 表单，不在 token 里。
        assert!(validated.encryption_version.is_none());
        // 节点绑定是新协议才有的概念；None 表示本节点。
        assert!(validated.node_id.is_none());
        assert!(validated.upload_base_url.is_none());
    }

    /// 派生只认**收到的那份凭证**。两者分家时若按记录里的字段派生，
    /// 锁、目录和完成幂等会一起指向另一个 upload_id。
    #[tokio::test]
    async fn the_upload_id_follows_the_credential_that_was_presented() {
        let service = UploadTokenService::new();
        let record = service
            .generate_token(
                7,
                FileType::File,
                1024,
                "message".to_string(),
                None,
                UploadIdentity::default(),
                UploadTokenPurpose::Upload,
            )
            .await
            .unwrap();

        let presented = "the-credential-the-client-actually-sent";
        let validated = ValidatedUploadToken::from_legacy(presented, &record);

        assert_eq!(validated.upload_id, derive_legacy_upload_id(presented));
        assert_ne!(validated.upload_id, derive_legacy_upload_id(&record.token));
    }
}
