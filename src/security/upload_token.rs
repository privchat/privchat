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

//! 自包含签名上传 token（RESUMABLE_UPLOAD_SPEC §5.2）。
//!
//! 形状照搬 `security::room_ticket`（独立用途、`kid` 多密钥、HS256、时钟宽限），
//! 但**是另一个 verifier**：room ticket 明确关掉了 `aud` 校验
//! （`room_ticket.rs` 的 `validation.validate_aud = false`），直接复用它会让本模块
//! 的 `typ` / `aud` 隔离形同虚设。
//!
//! 与登录 JWT 也互不通用：`typ=upload`（JWT header）+ `aud=file-upload`（claims），
//! 两侧都强制校验。

use jsonwebtoken::{
    decode, decode_header, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::service::upload_token_service::{UploadPlan, UploadTokenPurpose};

/// JWT header `typ`。与登录 token 分家的第一道闸。
pub const TOKEN_TYP: &str = "upload";
/// claims `aud`。第二道闸，verifier 必须开启 `validate_aud` 才有意义。
pub const TOKEN_AUD: &str = "file-upload";
/// 当前 claims 版本。
pub const CLAIMS_VERSION: u8 = 1;

/// 有效期硬上限（秒）：24 小时。
pub const MAX_TTL_SECS: u64 = 86_400;

/// 展示文件名的字节上限。
///
/// 🔴 这是**体积预算**逼出来的约束，不是美观要求：token 跟着每一个分片请求走，
/// 而 `original_filename` 列是 `VARCHAR(512)`。真放 512 字节进去，单张 token 就会
/// 突破 1KB 预算，弱网上每个 64KiB 请求都要多背这一份。255 是 POSIX `NAME_MAX`，
/// 对展示名绰绰有余。
pub const MAX_FILENAME_BYTES: usize = 255;

/// 典型 token 的体积预算（字节）。
///
/// 「典型」= 手机发一张照片：短文件名、常见 MIME、单节点无 base_url。
/// 这一档决定日常开销：64KiB 请求下约 0.9%。
pub const TYPICAL_TOKEN_BUDGET_BYTES: usize = 700;

/// 最大合法 token 的体积预算（字节）。
///
/// 🔴 **实测：1KB 装不下最大合法载荷。** 满额 255 字节 UTF-8 文件名经 base64url
/// 膨胀约 1.33 倍就要 ~340 字节，加上 64 字节摘要、64 字节 upload_id、长 MIME 与
/// 节点信息，实测约 1.2KB。与其把断言凑到 1KB，不如按实测把上限定在这里：
/// 64KiB 请求下最坏约 1.9% 开销，仍然可接受。
///
/// 生产 nginx 未配 `large_client_header_buffers`，默认 `client_header_buffer_size 1k`
/// + `large_client_header_buffers 4 8k`：**典型 token 落在小缓冲内**，只有最坏情况
/// 会多分配一次大缓冲，不会失败。
pub const MAX_TOKEN_BUDGET_BYTES: usize = 1400;

/// 签发模式（RESUMABLE_UPLOAD_SPEC §5.2.4 回滚开关）。
///
/// 🔴 验证侧**始终双验**，与本开关无关；开关只管「新 token 签成什么格式」。
/// 配置无热更（`ServerConfig::load` 只在启动时跑），所以回滚动作是改配置 + 重启。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueMode {
    /// 维持现状：签旧的 Redis UUID token。
    LegacyUuid,
    /// 签自包含 token。
    Signed,
}

impl Default for IssueMode {
    /// 默认不改变现有行为——上线部署那一刻不应该顺便切换 token 格式。
    fn default() -> Self {
        Self::LegacyUuid
    }
}

/// `[upload.token]` 配置。
#[derive(Debug, Clone)]
pub struct UploadTokenConfig {
    /// kid → secret。
    pub keys: HashMap<String, String>,
    /// 签发时用哪个 kid。
    pub default_kid: String,
    /// 时钟宽限（秒）。
    pub leeway_secs: u64,
    /// 签发有效期（秒），不得超过 [`MAX_TTL_SECS`]。
    pub ttl_secs: u64,
    pub issue_mode: IssueMode,
}

impl UploadTokenConfig {
    pub fn resolve_secret(&self, kid: Option<&str>) -> Option<&String> {
        self.keys.get(kid.unwrap_or(&self.default_kid))
    }
}

/// `UploadPlan` 在 **token 里**的紧凑形态。
///
/// 🔴 与 [`UploadPlan`] 分开正是「签名 claims ≠ 响应模型」那条规矩的落点：
/// API 响应要可读的全名（客户端契约），token 要短名（它跟着每一个分片请求走，
/// 五个全名 key 就要 ~90 字节，base64 后 ~120）。两者用 `From` 互转，加字段时
/// 也不会因为改了响应体就悄悄把 token 撑大。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PlanClaims {
    bu: u32,
    ir: u32,
    mr: u32,
    st: u64,
    mp: u8,
}

impl From<&UploadPlan> for PlanClaims {
    fn from(p: &UploadPlan) -> Self {
        Self {
            bu: p.base_unit,
            ir: p.initial_request_size,
            mr: p.max_request_size,
            st: p.session_threshold,
            mp: p.max_parallel_parts,
        }
    }
}

impl From<&PlanClaims> for UploadPlan {
    fn from(p: &PlanClaims) -> Self {
        Self {
            base_unit: p.bu,
            initial_request_size: p.ir,
            max_request_size: p.mr,
            session_threshold: p.st,
            max_parallel_parts: p.mp,
        }
    }
}

/// 签进 token 的冻结事实。
///
/// 字段名刻意压短：token 要跟着每一个分片请求走，1KB 的 token 在 64KiB 请求下
/// 就是 1.5% 的固定开销（RESUMABLE_UPLOAD_SPEC §5.2.2）。缺省项一律不序列化。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadTokenClaims {
    /// claims 版本。
    pub v: u8,
    /// 🔴 `jti` **就是** `upload_id`：会话目录名、模式锁与完成幂等键的轴。
    /// 一个字段兼两个角色，省掉一份 32 字节的重复。
    #[serde(rename = "jti")]
    pub upload_id: String,
    /// 受众；verifier 必须校验（见模块注释）。
    pub aud: String,
    pub uid: u64,
    /// 用途：`"u"` = 传字节，`"c"` = 换 file_id。
    pub prp: String,
    /// 文件类型（`FileType` 的 wire 名）。
    pub ft: String,
    pub bt: String,
    pub mx: i64,
    pub tv: i32,
    #[serde(rename = "fnm", skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    #[serde(rename = "sh", skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(rename = "sz", skip_serializing_if = "Option::is_none")]
    pub sealed_blob_size: Option<i64>,
    #[serde(rename = "mt", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(rename = "ev", skip_serializing_if = "Option::is_none")]
    pub encryption_version: Option<i32>,
    #[serde(rename = "nd", skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(rename = "url", skip_serializing_if = "Option::is_none")]
    pub upload_base_url: Option<String>,
    #[serde(rename = "pl", skip_serializing_if = "Option::is_none")]
    plan: Option<PlanClaims>,
    pub iat: u64,
    pub exp: u64,
}

impl UploadTokenClaims {
    /// 读出上传方案（转回可读模型）。
    pub fn upload_plan(&self) -> Option<UploadPlan> {
        self.plan.as_ref().map(UploadPlan::from)
    }

    /// 写入上传方案（转成紧凑形态）。
    pub fn set_upload_plan(&mut self, plan: Option<&UploadPlan>) {
        self.plan = plan.map(PlanClaims::from);
    }

    pub fn purpose(&self) -> Option<UploadTokenPurpose> {
        match self.prp.as_str() {
            "u" => Some(UploadTokenPurpose::Upload),
            "c" => Some(UploadTokenPurpose::ClaimExisting),
            _ => None,
        }
    }

    pub fn purpose_code(p: UploadTokenPurpose) -> &'static str {
        match p {
            UploadTokenPurpose::Upload => "u",
            UploadTokenPurpose::ClaimExisting => "c",
        }
    }
}

/// 验证失败原因。分开是为了让日志能说清是哪一道闸拦的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadTokenError {
    MalformedHeader,
    /// header `typ` 不是 [`TOKEN_TYP`]——多半是拿登录 token 来调上传端点。
    WrongTyp,
    UnknownKid,
    InvalidSignature,
    Expired,
    /// `aud` 不是 [`TOKEN_AUD`]。
    WrongAudience,
    /// `prp` 不是已知用途。
    BadPurpose,
    /// `exp - iat` 超过 24h + 宽限：签发侧配错了，不能因为「签名对」就接受。
    TtlTooLong,
    /// claims 版本不认识。
    UnknownVersion,
    Other,
}

impl std::fmt::Display for UploadTokenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::MalformedHeader => "malformed_header",
            Self::WrongTyp => "wrong_typ",
            Self::UnknownKid => "unknown_kid",
            Self::InvalidSignature => "invalid_signature",
            Self::Expired => "expired",
            Self::WrongAudience => "wrong_audience",
            Self::BadPurpose => "bad_purpose",
            Self::TtlTooLong => "ttl_too_long",
            Self::UnknownVersion => "unknown_version",
            Self::Other => "other",
        })
    }
}

/// 签发前的不变量检查。签名只能证明「这串字节是我发的」，证明不了内容合理。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueError {
    UnknownKid,
    /// 配置的 `ttl_secs` 超过 24h 上限。
    TtlTooLong,
    /// 声明大小必须为正——`0` 会让完成时的大小复核变成「和 0 比对」。
    NonPositiveSize,
    FilenameTooLong,
    /// `UploadPlan` 内部不自洽，见 [`validate_plan`]。
    BadUploadPlan,
    EncodeFailed,
}

impl std::fmt::Display for IssueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::UnknownKid => "unknown_kid",
            Self::TtlTooLong => "ttl_too_long",
            Self::NonPositiveSize => "non_positive_size",
            Self::FilenameTooLong => "filename_too_long",
            Self::BadUploadPlan => "bad_upload_plan",
            Self::EncodeFailed => "encode_failed",
        })
    }
}

/// `UploadPlan` 的自洽性。
///
/// 这些值一旦签进 token 就冻结在整个上传上，事后改不了；签发时不查，
/// 错误配置会一路带到客户端的分片循环里去。
pub fn validate_plan(plan: &UploadPlan) -> Result<(), IssueError> {
    let bad = plan.base_unit == 0
        || plan.initial_request_size == 0
        || plan.max_request_size == 0
        || plan.max_parallel_parts == 0
        // 区间寻址按 base_unit 对齐；不对齐的请求大小根本发不出合法区间。
        || plan.initial_request_size % plan.base_unit != 0
        || plan.max_request_size % plan.base_unit != 0
        // 初始值不能比上限还大。
        || plan.initial_request_size > plan.max_request_size
        // 并发上限给个合理范围：spec §3.1 建议稳定后 2、最多 3。
        || plan.max_parallel_parts > 8;
    if bad {
        return Err(IssueError::BadUploadPlan);
    }
    Ok(())
}

/// 签发一张自包含 upload token。
pub fn sign(
    cfg: &UploadTokenConfig,
    now_secs: u64,
    mut claims: UploadTokenClaims,
) -> Result<String, IssueError> {
    if cfg.ttl_secs > MAX_TTL_SECS {
        return Err(IssueError::TtlTooLong);
    }
    if let Some(size) = claims.sealed_blob_size {
        if size <= 0 {
            return Err(IssueError::NonPositiveSize);
        }
    }
    if let Some(name) = &claims.filename {
        if name.len() > MAX_FILENAME_BYTES {
            return Err(IssueError::FilenameTooLong);
        }
    }
    if let Some(plan) = claims.upload_plan() {
        validate_plan(&plan)?;
    }

    claims.v = CLAIMS_VERSION;
    claims.aud = TOKEN_AUD.to_string();
    claims.iat = now_secs;
    claims.exp = now_secs.saturating_add(cfg.ttl_secs);

    let kid = cfg.default_kid.clone();
    let secret = cfg.resolve_secret(Some(&kid)).ok_or(IssueError::UnknownKid)?;

    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some(kid);
    // 🔴 `typ` 走 header：不解析 claims 就能把登录 token 挡在门外。
    header.typ = Some(TOKEN_TYP.to_string());

    encode(&header, &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(|_| IssueError::EncodeFailed)
}

/// 这串字符串**看起来**像不像一张 JWT。
///
/// 🔴 用途只有一个：把「签名 token 验失败」与「这压根不是签名 token」分开。
/// 迁移期两种格式并存，若不区分，一张签名坏了 / `kid` 错 / `aud` 错的 token
/// 会掉进旧 Redis 查询分支——那等于给攻击者一条**降级**通道。
pub fn looks_like_signed(token: &str) -> bool {
    token.split('.').count() == 3
}

/// 验证一张自包含 upload token。
///
/// 🔴 只做「这张 token 说了什么」，**不做**「这次请求能不能干这件事」：
/// 用途匹配、uid 归属、节点归属由调用方按端点各自判定。
pub fn verify(
    cfg: &UploadTokenConfig,
    token: &str,
) -> Result<UploadTokenClaims, UploadTokenError> {
    let header = decode_header(token).map_err(|_| UploadTokenError::MalformedHeader)?;
    if header.alg != Algorithm::HS256 {
        return Err(UploadTokenError::MalformedHeader);
    }
    // header 里就把非上传 token 挡掉，不必先验签。
    if header.typ.as_deref() != Some(TOKEN_TYP) {
        return Err(UploadTokenError::WrongTyp);
    }
    let secret = cfg
        .resolve_secret(header.kid.as_deref())
        .ok_or(UploadTokenError::UnknownKid)?;

    let mut validation = Validation::new(Algorithm::HS256);
    validation.leeway = cfg.leeway_secs;
    validation.set_required_spec_claims(&["exp", "aud"]);
    // 🔴 与 room_ticket 相反：这里必须开。关掉 `aud` 校验，`typ`/`aud` 隔离就没了。
    validation.validate_aud = true;
    validation.set_audience(&[TOKEN_AUD]);

    let data = decode::<UploadTokenClaims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => UploadTokenError::Expired,
        jsonwebtoken::errors::ErrorKind::InvalidSignature => UploadTokenError::InvalidSignature,
        jsonwebtoken::errors::ErrorKind::InvalidAudience => UploadTokenError::WrongAudience,
        _ => UploadTokenError::Other,
    })?;
    let claims = data.claims;

    if claims.v != CLAIMS_VERSION {
        return Err(UploadTokenError::UnknownVersion);
    }
    if claims.purpose().is_none() {
        return Err(UploadTokenError::BadPurpose);
    }
    // 🔴 签名对不代表有效期合理：签发侧配错 `ttl_secs`（或换了实现）就可能签出
    // 几年有效的 token。验证侧独立复核一遍上限。
    if claims.exp.saturating_sub(claims.iat) > MAX_TTL_SECS.saturating_add(cfg.leeway_secs) {
        return Err(UploadTokenError::TtlTooLong);
    }

    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试用「现在」。写死一个过去的时间戳会让每张 token 一签出来就是过期的，
    /// 于是所有断言都撞在 `Expired` 上，测不到真正想测的那道闸。
    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn cfg() -> UploadTokenConfig {
        let mut keys = HashMap::new();
        keys.insert("upload-v1".to_string(), "s3cr3t-current".to_string());
        keys.insert("upload-v0".to_string(), "s3cr3t-previous".to_string());
        UploadTokenConfig {
            keys,
            default_kid: "upload-v1".to_string(),
            leeway_secs: 30,
            ttl_secs: MAX_TTL_SECS,
            issue_mode: IssueMode::Signed,
        }
    }

    fn plan() -> UploadPlan {
        UploadPlan {
            base_unit: 65536,
            initial_request_size: 65536,
            max_request_size: 2 * 1024 * 1024,
            session_threshold: 65536,
            max_parallel_parts: 3,
        }
    }

    fn claims() -> UploadTokenClaims {
        UploadTokenClaims {
            v: CLAIMS_VERSION,
            upload_id: "0".repeat(32),
            aud: TOKEN_AUD.to_string(),
            uid: 100_002_319,
            prp: "u".to_string(),
            ft: "image".to_string(),
            bt: "message".to_string(),
            mx: 200 * 1024 * 1024,
            tv: 1,
            filename: Some("holiday.png".to_string()),
            sha256: Some("a".repeat(64)),
            sealed_blob_size: Some(73_400_320),
            mime_type: Some("image/png".to_string()),
            encryption_version: Some(1),
            node_id: None,
            upload_base_url: None,
            plan: Some(PlanClaims::from(&plan())),
            iat: 0,
            exp: 0,
        }
    }

    #[test]
    fn a_signed_token_round_trips() {
        let c = cfg();
        let token = sign(&c, now(), claims()).expect("sign");
        let back = verify(&c, &token).expect("verify");
        assert_eq!(back.upload_id, "0".repeat(32));
        assert_eq!(back.uid, 100_002_319);
        assert_eq!(back.purpose(), Some(UploadTokenPurpose::Upload));
        assert_eq!(back.sealed_blob_size, Some(73_400_320));
        assert_eq!(back.upload_plan().map(|p| p.base_unit), Some(65536));
        assert_eq!(back.exp - back.iat, MAX_TTL_SECS);
    }

    /// 轮换期的核心保证：用旧密钥签发、尚未过期的 token 必须还能验过，
    /// 否则一次换密钥就会打断所有在途上传。
    #[test]
    fn a_token_signed_with_the_previous_key_still_verifies() {
        let mut c = cfg();
        c.default_kid = "upload-v0".to_string();
        let token = sign(&c, now(), claims()).expect("sign");

        // 服务端已经轮到新 kid，但旧验证密钥仍在配置里。
        let mut rotated = cfg();
        rotated.default_kid = "upload-v1".to_string();
        assert!(verify(&rotated, &token).is_ok());
    }

    #[test]
    fn an_unknown_kid_is_rejected() {
        let c = cfg();
        let token = sign(&c, now(), claims()).expect("sign");
        let mut stripped = cfg();
        stripped.keys.remove("upload-v1");
        assert_eq!(verify(&stripped, &token).unwrap_err(), UploadTokenError::UnknownKid);
    }

    #[test]
    fn a_tampered_signature_is_rejected() {
        let c = cfg();
        let token = sign(&c, now(), claims()).expect("sign");
        let mut forged = cfg();
        forged
            .keys
            .insert("upload-v1".to_string(), "another-secret".to_string());
        assert_eq!(verify(&forged, &token).unwrap_err(), UploadTokenError::InvalidSignature);
    }

    /// `typ` 是与登录 token 分家的第一道闸：没有它，任何一张同密钥域的 JWT
    /// 都能来调上传端点。
    #[test]
    fn a_token_without_the_upload_typ_is_rejected() {
        let c = cfg();
        let secret = c.resolve_secret(Some("upload-v1")).unwrap().clone();
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("upload-v1".to_string());
        header.typ = Some("JWT".to_string()); // 普通 JWT
        let mut cl = claims();
        cl.iat = now();
        cl.exp = cl.iat + 600;
        let token = encode(&header, &cl, &EncodingKey::from_secret(secret.as_bytes())).unwrap();
        assert_eq!(verify(&c, &token).unwrap_err(), UploadTokenError::WrongTyp);
    }

    /// `aud` 是第二道闸。room_ticket 的 verifier 关掉了它——这条测试就是
    /// 「不要顺手复用那个 verifier」的守卫。
    #[test]
    fn a_token_for_another_audience_is_rejected() {
        let c = cfg();
        let secret = c.resolve_secret(Some("upload-v1")).unwrap().clone();
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("upload-v1".to_string());
        header.typ = Some(TOKEN_TYP.to_string());
        let mut cl = claims();
        cl.aud = "some-other-service".to_string();
        cl.iat = now();
        cl.exp = cl.iat + 600;
        let token = encode(&header, &cl, &EncodingKey::from_secret(secret.as_bytes())).unwrap();
        assert_eq!(verify(&c, &token).unwrap_err(), UploadTokenError::WrongAudience);
    }

    /// 签名对不代表有效期合理：签发侧配错就可能签出超长期 token，
    /// 验证侧必须独立复核上限。
    #[test]
    fn an_overlong_ttl_is_rejected_even_with_a_valid_signature() {
        let c = cfg();
        let secret = c.resolve_secret(Some("upload-v1")).unwrap().clone();
        let mut header = Header::new(Algorithm::HS256);
        header.kid = Some("upload-v1".to_string());
        header.typ = Some(TOKEN_TYP.to_string());
        let mut cl = claims();
        cl.iat = now();
        cl.exp = cl.iat + MAX_TTL_SECS * 365; // 一年
        let token = encode(&header, &cl, &EncodingKey::from_secret(secret.as_bytes())).unwrap();
        assert_eq!(verify(&c, &token).unwrap_err(), UploadTokenError::TtlTooLong);
    }

    #[test]
    fn signing_refuses_a_ttl_above_the_hard_cap() {
        let mut c = cfg();
        c.ttl_secs = MAX_TTL_SECS + 1;
        assert_eq!(sign(&c, now(), claims()).unwrap_err(), IssueError::TtlTooLong);
    }

    /// 大小为 0 会让完成时的「声明 vs 实际」复核退化成和 0 比对。
    #[test]
    fn signing_refuses_a_non_positive_size() {
        let c = cfg();
        let mut cl = claims();
        cl.sealed_blob_size = Some(0);
        assert_eq!(sign(&c, 0, cl).unwrap_err(), IssueError::NonPositiveSize);
    }

    #[test]
    fn signing_refuses_an_overlong_filename() {
        let c = cfg();
        let mut cl = claims();
        cl.filename = Some("x".repeat(MAX_FILENAME_BYTES + 1));
        assert_eq!(sign(&c, 0, cl).unwrap_err(), IssueError::FilenameTooLong);
    }

    /// 方案一旦签进 token 就冻结在整个上传上，签发时不查就会一路带到客户端的
    /// 分片循环里。
    #[test]
    fn an_inconsistent_upload_plan_is_refused() {
        let base = plan();

        let mut zero_unit = base.clone();
        zero_unit.base_unit = 0;
        assert!(validate_plan(&zero_unit).is_err());

        let mut unaligned = base.clone();
        unaligned.max_request_size = 65536 * 3 + 1;
        assert!(validate_plan(&unaligned).is_err());

        let mut inverted = base.clone();
        inverted.initial_request_size = 4 * 1024 * 1024;
        assert!(validate_plan(&inverted).is_err());

        let mut no_parallel = base.clone();
        no_parallel.max_parallel_parts = 0;
        assert!(validate_plan(&no_parallel).is_err());

        assert!(validate_plan(&base).is_ok());
    }

    /// 🔴 降级通道：签名坏了 / kid 错 / aud 错的 token **看起来仍然像 JWT**，
    /// 必须直接拒绝，不能掉进旧 Redis 查询分支。只有形如 UUID 的字符串才配走回退。
    #[test]
    fn a_broken_signed_token_must_not_look_like_a_legacy_one() {
        let c = cfg();
        let token = sign(&c, now(), claims()).expect("sign");
        assert!(looks_like_signed(&token));

        let mut tampered = token.clone();
        tampered.push('x'); // 破坏签名
        assert!(looks_like_signed(&tampered));

        // 旧 token 是 UUID，不含两个点。
        assert!(!looks_like_signed("b3f1a2c4-0000-4000-8000-000000000001"));
    }

    /// token 跟着每一个分片请求走：全程 64KiB 的 200MiB 文件是 3200 次请求，
    /// 每 100 字节 token 就是 320KB 纯 header 开销，而跑 64KiB 的正是上行最差的用户。
    /// 预算写在文档里没人守，写成断言才守得住。
    ///
    /// 两档分别断言：**典型**决定日常开销，**最大合法**决定最坏情况和代理缓冲。
    #[test]
    fn the_token_stays_within_its_size_budget() {
        let c = cfg();

        // 典型：手机相册发一张图。
        let mut typical = claims();
        typical.upload_id = "0".repeat(32);
        typical.filename = Some("IMG_20260813_162930.jpg".to_string());
        typical.mime_type = Some("image/jpeg".to_string());
        typical.node_id = None;
        typical.upload_base_url = None;
        let typical_len = sign(&c, now(), typical).expect("sign").len();

        // 最大合法：各字段取上限。
        let mut worst = claims();
        worst.upload_id = "f".repeat(64);
        worst.filename = Some("名".repeat(MAX_FILENAME_BYTES / 3));
        worst.sha256 = Some("f".repeat(64));
        worst.mime_type = Some(
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string(),
        );
        worst.bt = "group_file".to_string();
        worst.node_id = Some("upload-node-99".to_string());
        worst.upload_base_url = Some("https://upload-99.fflunp.cn".to_string());
        worst.encryption_version = Some(2);
        let worst_len = sign(&c, now(), worst).expect("sign").len();

        println!("token bytes: typical={typical_len} worst={worst_len}");
        assert!(
            typical_len <= TYPICAL_TOKEN_BUDGET_BYTES,
            "典型 token {typical_len} 字节，超出预算 {TYPICAL_TOKEN_BUDGET_BYTES}"
        );
        assert!(
            worst_len <= MAX_TOKEN_BUDGET_BYTES,
            "最大合法 token {worst_len} 字节，超出预算 {MAX_TOKEN_BUDGET_BYTES}"
        );
    }
}
