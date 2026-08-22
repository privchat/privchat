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

//! 分片上传会话（RESUMABLE_UPLOAD_SPEC，冻结于 privchat-docs `bdef282`）。
//!
//! **能用文件系统表达的，就不要再造一层记账。**
//!
//! ```text
//! tmp/uploads/chunked/{upload_id}/
//!   manifest.json        # 冻结事实（申请 token 时一次写成，之后只读；
//!                        #   唯一例外：part-url 签发后增量写入的 part_digests，第二十九轮）
//!   session.lock         # flock：文件操作互斥，不是状态机
//!   parts/{offset}-{length}.part
//!   body.complete.tmp    # complete 拼接的中间文件
//!   completed.json       # 完成墓碑：{ "file_id": n }
//! ```
//!
//! - **一片一文件**：进度 = 扫目录；没有 bitmap、journal、游标。
//! - **乱序**：complete 时按 offset 排序拼接。
//! - **随机 opaque token** `{upload_id}.{secret}`：manifest 只存 `SHA-256(secret)`。
//! - **文件存在即字节可信**：每片 fsync 之后才 rename 成正式名，再 fsync 目录。
//!
//! 与整包会话（`upload_session.rs`，`tmp/uploads/{uid}/{upload_id}`）放在同一棵根下的
//! `chunked/` 子目录里，两套目录形态互不干扰。

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::error::{Result, ServerError};

/// `upload_id`：128-bit 随机值的十六进制。
pub const UPLOAD_ID_HEX_LEN: usize = 32;
/// `secret`：256-bit 随机值的十六进制。
pub const SECRET_HEX_LEN: usize = 64;
/// token 有效期：24 小时，不滑动。
pub const TOKEN_TTL_SECS: i64 = 24 * 3600;
/// 寻址网格：64KiB。
pub const BASE_UNIT: u32 = 64 * 1024;
/// 单次分片请求硬上限（路由层 body limit）。
pub const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// 上传数据面标识（RESUMABLE_UPLOAD_SPEC §8.1）：现有内置分片协议，一行不动。
pub const TRANSPORT_PROXY_OFFSET_V1: &str = "proxy_offset_v1";
/// 上传数据面标识（RESUMABLE_UPLOAD_SPEC §8）：S3 原生 Multipart 直传（待实现）。
pub const TRANSPORT_S3_MULTIPART_V1: &str = "s3_multipart_v1";

/// 模式选择失败（单一数据面，第二十轮评审用户规则）：
/// - `SetMissingProxy`：proxy 数据面下声明集合缺少 `proxy_offset_v1`；
/// - `ServerS3Only`：服务端配置了 S3 单一数据面，客户端未声明 `s3_multipart_v1`。
///   🔴 不得回退/兜底——只能报错，客户端升级或管理员改配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportSelectError {
    SetMissingProxy,
    ServerS3Only,
}

/// S3 直传门禁输入（RESUMABLE_UPLOAD_SPEC §8.2）：`open` = 默认存储源显式
/// `direct_upload` 配置 + 后端已接线。🔴 单一数据面（第二十轮）：配置单选，
/// 没有阈值/回退/能力协商——配了 S3 则全部会话走 S3，没配则全部走 proxy。
/// 判定全部收敛在 [`select_transport`] 内，不得在别的处另写判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct S3DirectGate {
    pub open: bool,
}

/// 单一数据面的模式选择（RESUMABLE_UPLOAD_SPEC §8.2，第二十轮评审）。
///
/// 🔴 规则由本函数自己强制，不靠调用方注释维持：
/// - 服务端配了 S3（`gate.open`）：token 只能绑 `s3_multipart_v1`。客户端声明了它 →
///   S3；否则（含旧客户端不带字段）→ `Err(ServerS3Only)` → 「不支持该上传模式」，
///   绝不静默改走内置上传服务。
/// - 服务端没配（唯一数据面是内置服务）：字段省略 = 旧客户端，隐式 proxy；字段存在则必须包含 `proxy_offset_v1`，否则 `Err(SetMissingProxy)`。
/// 🔴 不存在「不达阈值回退」「未声明能力自动 proxy」「失败后切换数据面」：
/// 失败只能报错或重新申请同一模式的 token。
pub fn select_transport(
    declared: Option<&[String]>,
    gate: &S3DirectGate,
) -> std::result::Result<&'static str, TransportSelectError> {
    if gate.open {
        return match declared {
            Some(list) if list.iter().any(|t| t == TRANSPORT_S3_MULTIPART_V1) => {
                Ok(TRANSPORT_S3_MULTIPART_V1)
            }
            _ => Err(TransportSelectError::ServerS3Only),
        };
    }
    match declared {
        // 字段省略：旧客户端，隐式 proxy；现有自适应分片逻辑完全不变。
        None => Ok(TRANSPORT_PROXY_OFFSET_V1),
        // 🔴 集合规则：不含 proxy_offset_v1 → 拒绝，调用方回参数错误。
        Some(list) if !list.iter().any(|t| t == TRANSPORT_PROXY_OFFSET_V1) => {
            Err(TransportSelectError::SetMissingProxy)
        }
        Some(_) => Ok(TRANSPORT_PROXY_OFFSET_V1),
    }
}

/// S3 固定分片几何（RESUMABLE §8.1 冻结公式）：
/// `part_size = align_up(max(8 MiB, ceil(size / 10000)), 1 MiB)`，限域 `[5 MiB, 5 GiB]`。
/// 返回 `(part_size, total_parts)`；末片长度由 `check_part_geometry` 按余数口径校验。
pub fn s3_part_geometry(total_size: u64) -> (u64, u32) {
    const MIN_PART: u64 = 5 << 20;
    const DEFAULT_PART: u64 = 8 << 20;
    const MAX_PART: u64 = 5 << 30;
    const MAX_PARTS: u64 = 10_000;
    const ALIGN: u64 = 1 << 20;
    let by_count = total_size.div_ceil(MAX_PARTS);
    let mut part_size = DEFAULT_PART.max(by_count);
    part_size = part_size.div_ceil(ALIGN) * ALIGN;
    part_size = part_size.clamp(MIN_PART, MAX_PART);
    let total_parts = total_size.div_ceil(part_size) as u32;
    (part_size, total_parts)
}

/// 冻结事实。申请 token 时一次写成；complete 建行所需的一切都从这里读。
///
/// 判据：本结构 ∪ complete 请求体（cek / business_id / encryption_version）必须覆盖
/// `file_service::RecordFields` 的每一个字段（`uploader_ip` 取自 complete 请求本身）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub secret_sha256: String,
    pub uploader_id: u64,
    pub total_size: u64,
    pub sealed_sha256: String,
    pub file_type: String,
    pub business_type: String,
    pub filename: String,
    pub mime_type: String,
    #[serde(default)]
    pub transform_version: i32,
    /// Unix 秒。
    pub expires_at: i64,
    /// 建会话时就预分配的 `file_id`（只取序列号，不产生 PG 临时行）。
    /// 它是「PG 已提交、墓碑还没写」这个崩溃窗口的恢复锚点。
    pub reserved_file_id: u64,
    /// 上传数据面（RESUMABLE §8.2）：`proxy_offset_v1` / `s3_multipart_v1`。
    /// 旧 manifest 没有该字段 → 默认 proxy（升级兼容），status/complete/abort
    /// 与 `/files/part-url` 都按它分流。
    #[serde(default = "default_manifest_transport")]
    pub transport: String,
    /// 仅 `s3_multipart_v1`：固定分片大小（字节）。
    #[serde(default)]
    pub part_size: Option<u64>,
    /// 仅 `s3_multipart_v1`：总分片数。
    #[serde(default)]
    pub total_parts: Option<u32>,
    /// 仅 `s3_multipart_v1`：S3 bucket（spec 冻结的 manifest 平铺字段）。
    #[serde(default)]
    pub bucket: Option<String>,
    /// 仅 `s3_multipart_v1`：最终对象 key（spec 字段名 `final_key`）。
    #[serde(default)]
    pub final_key: Option<String>,
    /// 仅 `s3_multipart_v1`：provider 的原始 MPU UploadId，不下发客户端。
    /// 🔴 三字段与 `part_size`/`total_parts` 作为整体原子使用（RESUMABLE §8.7），
    /// 进程重启后靠它们恢复控制面操作，不依赖进程内映射。
    #[serde(default)]
    pub provider_upload_id: Option<String>,
    /// 仅 `s3_multipart_v1`：🔴 建会话时冻结的存储源 id（第十五轮评审 P0）。
    /// 建行按该值校验 bucket 并落库，绝不重新读取当前默认存储源——会话可存活
    /// 24 小时，期间配置切换/重启不得改变这份上传最终指向的后端。
    #[serde(default)]
    pub storage_source_id: Option<u32>,
    #[serde(default)]
    pub created_at: i64,
    /// 仅 `s3_multipart_v1`（第二十九轮 COS 最小兼容）：`part_number` →
    /// part-url 签发时客户端声明的片摘要（RFC 4648 标准 Base64）。同一片号最新
    /// 声明覆盖旧值。它是 Complete 体逐片 checksum 的组装来源（§8.5 第 4 步）：
    /// COS 的 ListParts 不回逐片摘要，本地声明是唯一来源。🔴 manifest 唯一允许
    /// 的增量写入（在 flock 内原子重写），其余字段仍一次写成后只读。
    #[serde(default)]
    pub part_digests: std::collections::BTreeMap<u32, String>,
}

fn default_manifest_transport() -> String {
    TRANSPORT_PROXY_OFFSET_V1.to_string()
}

/// 完成墓碑。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Completed {
    pub file_id: u64,
}

/// 一段区间 `[offset, offset+length)`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Range {
    pub offset: u64,
    pub length: u64,
}

/// `PUT chunk` 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartOutcome {
    /// 这次真的写进去了。
    Written,
    /// 同边界同内容的 part 已存在：上次写成功但响应丢了；磁盘不动。
    AlreadyPresent,
}

/// 打开会话失败的三种。
#[derive(Debug)]
pub enum OpenError {
    /// token 形状不对。
    Malformed,
    /// 目录不在（过期已清 / abort / 从未存在）。
    Gone,
    /// secret 不符：**与 Gone 同一句话回给客户端**，不做存在性探测器。
    BadSecret,
    /// 已过 `expires_at`。
    Expired,
    Io(ServerError),
}

impl From<ServerError> for OpenError {
    fn from(e: ServerError) -> Self {
        OpenError::Io(e)
    }
}

/// 一个分片会话目录。持有它不等于持锁；锁见 [`ChunkedSession::lock`]。
pub struct ChunkedSession {
    upload_id: String,
    dir: PathBuf,
    manifest: Manifest,
}

/// 持锁守卫：drop 即释放（进程被杀由内核释放）。
pub struct SessionLock {
    _file: File,
}

/// `chunked/` 子目录。
pub fn chunked_root(session_root: &Path) -> PathBuf {
    session_root.join("chunked")
}

/// 拆 token。只看形状，不做任何验证。
pub fn parse_token(raw: &str) -> Option<(&str, &str)> {
    let (id, secret) = raw.trim().split_once('.')?;
    let ok = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_hexdigit());
    if ok(id, UPLOAD_ID_HEX_LEN) && ok(secret, SECRET_HEX_LEN) {
        Some((id, secret))
    } else {
        None
    }
}

/// 这串凭证长得像分片 token 吗（用来在几种凭证形态之间分流）。
pub fn looks_like_chunked_token(raw: &str) -> bool {
    parse_token(raw).is_some()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(sha2::Sha256::digest(bytes))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 原子写一个小 JSON：临时名 → 写 → fsync → rename → fsync 目录。
fn write_json_atomic<T: Serialize>(dir: &Path, name: &str, value: &T) -> Result<()> {
    let tmp = dir.join(format!(".{name}.tmp"));
    let bytes = serde_json::to_vec(value)
        .map_err(|e| ServerError::Internal(format!("序列化 {name} 失败: {e}")))?;
    {
        let mut f = File::create(&tmp)
            .map_err(|e| ServerError::Internal(format!("写 {name} 失败: {e}")))?;
        f.write_all(&bytes)
            .map_err(|e| ServerError::Internal(format!("写 {name} 失败: {e}")))?;
        f.sync_all()
            .map_err(|e| ServerError::Internal(format!("同步 {name} 失败: {e}")))?;
    }
    std::fs::rename(&tmp, dir.join(name))
        .map_err(|e| ServerError::Internal(format!("替换 {name} 失败: {e}")))?;
    fsync_dir(dir)
}

fn fsync_dir(dir: &Path) -> Result<()> {
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| ServerError::Internal(format!("同步目录 {dir:?} 失败: {e}")))
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| ServerError::Internal(format!("{path:?} 损坏: {e}"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ServerError::Internal(format!("读 {path:?} 失败: {e}"))),
    }
}

/// 建会话时的输入：除 secret / id / 时间之外的全部冻结事实。
pub struct NewSession {
    pub uploader_id: u64,
    pub total_size: u64,
    pub sealed_sha256: String,
    pub file_type: String,
    pub business_type: String,
    pub filename: String,
    pub mime_type: String,
    pub transform_version: i32,
    pub reserved_file_id: u64,
    /// 协商选定的数据面（RESUMABLE §8.2）。
    pub transport: String,
    /// 仅 `s3_multipart_v1`：建会话时一次写齐的冻结字段（含 `storage_source_id`，
    /// RESUMABLE §3.2）。proxy 会话恒 `None`。
    pub s3: Option<S3SessionSetup>,
}

/// S3 会话的冻结字段（RESUMABLE §3.2）：建会话时一次写成，complete 建行按它校验，
/// 🔴 绝不重新读取当前默认存储源。`provider_upload_id` 来自建会话前的
/// `CreateMultipartUpload`（§2.2：先建 MPU 再写 manifest）。
pub struct S3SessionSetup {
    pub part_size: u64,
    pub total_parts: u32,
    pub bucket: String,
    pub final_key: String,
    pub provider_upload_id: String,
    pub storage_source_id: u32,
}

/// 预生成的会话身份（第十六轮评审 P0）：S3 签发链路必须在写 manifest **之前**
/// 拿 `CreateMultipartUpload`（对象 metadata 要写 `privchat-upload-id = session_id`，
/// RESUMABLE §2.2），所以 id/secret 的生成从建目录里剖出来。
pub struct SessionIds {
    pub upload_id: String,
    pub secret: String,
}

pub fn new_session_ids() -> SessionIds {
    SessionIds {
        upload_id: hex::encode(rand::random::<[u8; 16]>()),
        secret: hex::encode(rand::random::<[u8; 32]>()),
    }
}

/// S3 恢复锚点（第十七轮评审 P1/P2）：`CreateMultipartUpload` 成功后**立即**落盘，
/// 在写 manifest 之前。不变式：**MPU 存在 ⇒ 锚点文件在或 manifest 可读**——
/// 据此扫描器对 manifest 损坏/缺失的目录能区分「可恢复的 S3 会话」与「可证 MPU
/// 从未创建的半建目录」。锁在 `chunked/` 之外的 `s3-anchors/`，避免被目录扫描误删。
/// 签发成功后删除（manifest 接管）；删不掉的残留由扫描器锚点 GC 收。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3Anchor {
    pub bucket: String,
    pub final_key: String,
    pub provider_upload_id: String,
    pub created_at: i64,
}

/// 锚点目录：`{session_root}/s3-anchors/`（与 `chunked/` 平级，不在其内）。
pub fn s3_anchor_root(session_root: &Path) -> PathBuf {
    session_root.join("s3-anchors")
}

fn s3_anchor_path(session_root: &Path, upload_id: &str) -> PathBuf {
    s3_anchor_root(session_root).join(format!("{upload_id}.json"))
}

/// 原子写锚点（目录不存在先建）。
pub fn write_s3_anchor(session_root: &Path, upload_id: &str, anchor: &S3Anchor) -> Result<()> {
    let root = s3_anchor_root(session_root);
    std::fs::create_dir_all(&root)
        .map_err(|e| ServerError::Internal(format!("创建锚点目录 {root:?} 失败: {e}")))?;
    write_json_atomic(&root, &format!("{upload_id}.json"), anchor)
}

pub fn read_s3_anchor(session_root: &Path, upload_id: &str) -> Result<Option<S3Anchor>> {
    read_json::<S3Anchor>(&s3_anchor_path(session_root, upload_id))
}

pub fn remove_s3_anchor(session_root: &Path, upload_id: &str) -> Result<()> {
    match std::fs::remove_file(s3_anchor_path(session_root, upload_id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ServerError::Internal(format!("删除锚点失败: {e}"))),
    }
}

impl ChunkedSession {
    /// 建目录 + 写 manifest，**成功之后才**返回 token。
    ///
    /// 返回 `(session, token, expires_at)`。
    pub fn create(session_root: &Path, input: NewSession) -> Result<(Self, String, i64)> {
        Self::create_with_ids(session_root, new_session_ids(), input)
    }

    /// 同 [`Self::create`]，但用调用方预生成的 [`SessionIds`]（S3 签发链路专用，
    /// 见其注释）。
    pub fn create_with_ids(
        session_root: &Path,
        ids: SessionIds,
        input: NewSession,
    ) -> Result<(Self, String, i64)> {
        let root = chunked_root(session_root);
        std::fs::create_dir_all(&root)
            .map_err(|e| ServerError::Internal(format!("创建 {root:?} 失败: {e}")))?;

        let upload_id = ids.upload_id;
        let secret = ids.secret;
        let dir = root.join(&upload_id);
        // 128-bit 随机值撞名的概率可以忽略；真撞了就是有人在造，拒绝而不是覆盖。
        std::fs::create_dir(&dir)
            .map_err(|e| ServerError::Internal(format!("创建会话目录失败: {e}")))?;
        std::fs::create_dir(dir.join("parts"))
            .map_err(|e| ServerError::Internal(format!("创建 parts 目录失败: {e}")))?;
        // 锁文件先建出来，之后 open 只 open 不 create。
        File::create(dir.join("session.lock"))
            .map_err(|e| ServerError::Internal(format!("创建会话锁失败: {e}")))?;

        let now = now_secs();
        // S3 冻结字段：建会话时一次写成（第十六轮评审 P0：真实签发链路写入，
        // 不再是测试夹具手改）；proxy 会话全 `None`。
        let s3 = input.s3.as_ref();
        let manifest = Manifest {
            secret_sha256: sha256_hex(secret.as_bytes()),
            uploader_id: input.uploader_id,
            total_size: input.total_size,
            sealed_sha256: input.sealed_sha256.to_ascii_lowercase(),
            file_type: input.file_type,
            business_type: input.business_type,
            filename: input.filename,
            mime_type: input.mime_type,
            transform_version: input.transform_version,
            expires_at: now + TOKEN_TTL_SECS,
            reserved_file_id: input.reserved_file_id,
            transport: input.transport,
            part_size: s3.map(|s| s.part_size),
            total_parts: s3.map(|s| s.total_parts),
            bucket: s3.map(|s| s.bucket.clone()),
            final_key: s3.map(|s| s.final_key.clone()),
            provider_upload_id: s3.map(|s| s.provider_upload_id.clone()),
            storage_source_id: s3.map(|s| s.storage_source_id),
            created_at: now,
            part_digests: std::collections::BTreeMap::new(),
        };
        write_json_atomic(&dir, "manifest.json", &manifest)?;
        // 会话目录项本身也要落盘。
        fsync_dir(&root)?;

        let expires_at = manifest.expires_at;
        let token = format!("{upload_id}.{secret}");
        Ok((
            Self {
                upload_id,
                dir,
                manifest,
            },
            token,
            expires_at,
        ))
    }

    /// 按 token 打开：拆 id 定位目录 → 算 SHA-256(secret) → 恒定时间比较 → 查过期。
    pub fn open(session_root: &Path, raw_token: &str) -> std::result::Result<Self, OpenError> {
        let (upload_id, secret) = parse_token(raw_token).ok_or(OpenError::Malformed)?;
        let dir = chunked_root(session_root).join(upload_id);
        if !dir.is_dir() {
            return Err(OpenError::Gone);
        }
        let manifest: Manifest = read_json(&dir.join("manifest.json"))?.ok_or(OpenError::Gone)?;
        let given = sha256_hex(secret.as_bytes());
        if !constant_time_eq(given.as_bytes(), manifest.secret_sha256.as_bytes()) {
            return Err(OpenError::BadSecret);
        }
        if now_secs() > manifest.expires_at {
            return Err(OpenError::Expired);
        }
        Ok(Self {
            upload_id: upload_id.to_string(),
            dir,
            manifest,
        })
    }

    pub fn upload_id(&self) -> &str {
        &self.upload_id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn parts_dir(&self) -> PathBuf {
        self.dir.join("parts")
    }

    pub fn assembled_path(&self) -> PathBuf {
        self.dir.join("body.complete.tmp")
    }

    /// 非阻塞排他锁；`None` = 被别人占着。
    pub fn try_lock(&self) -> Result<Option<SessionLock>> {
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.dir.join("session.lock"))
            .map_err(|e| ServerError::Internal(format!("打开会话锁失败: {e}")))?;
        match flock_nb(&f) {
            Ok(true) => Ok(Some(SessionLock { _file: f })),
            Ok(false) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// 带短暂等待的排他锁：同一客户端并发恒为 1，撞锁只会是上一请求还没收尾。
    /// 等一小会儿比直接回 409 让客户端再绕一圈便宜。
    pub async fn lock(&self, wait: std::time::Duration) -> Result<Option<SessionLock>> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(l) = self.try_lock()? {
                return Ok(Some(l));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }

    /// 已完成？
    pub fn completed_file_id(&self) -> Result<Option<u64>> {
        Ok(read_json::<Completed>(&self.dir.join("completed.json"))?.map(|c| c.file_id))
    }

    /// 写墓碑（原子 + fsync 目录）。
    pub fn write_completed(&self, file_id: u64) -> Result<()> {
        write_json_atomic(&self.dir, "completed.json", &Completed { file_id })
    }

    /// 记录逐片摘要声明（第二十九轮）：part-url 签发成功后调用（调用方持锁）。
    /// 同一片号最新声明覆盖旧值（URL 过期重拉时可能带新声明）。🔴 写失败必须报错：
    /// Complete 体依赖这些声明组装，丢下来 complete 永远过不去；此刻客户端还没拿到
    /// URL、未传任何字节，报错重试无损。
    pub fn record_part_digests(&mut self, decls: &[(u32, String)]) -> Result<()> {
        for (part_number, b64) in decls {
            self.manifest.part_digests.insert(*part_number, b64.clone());
        }
        write_json_atomic(&self.dir, "manifest.json", &self.manifest)
    }

    /// 墓碑之后：删 parts 与拼接中间文件，只留 manifest + completed.json。
    pub fn drop_payload(&self) {
        let _ = std::fs::remove_dir_all(self.parts_dir());
        let _ = std::fs::remove_file(self.assembled_path());
    }

    /// 扫描 `parts/`，返回按 offset 排序的区间（不合并）。
    pub fn scan_parts(&self) -> Result<Vec<Range>> {
        let mut out = Vec::new();
        let rd = match std::fs::read_dir(self.parts_dir()) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(ServerError::Internal(format!("扫描 parts 失败: {e}"))),
        };
        for entry in rd {
            let entry = entry.map_err(|e| ServerError::Internal(format!("扫描 parts 失败: {e}")))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(stem) = name.strip_suffix(".part") else { continue };
            let Some((o, l)) = stem.split_once('-') else { continue };
            if let (Ok(offset), Ok(length)) = (o.parse::<u64>(), l.parse::<u64>()) {
                out.push(Range { offset, length });
            }
        }
        out.sort_by_key(|r| r.offset);
        Ok(out)
    }

    /// 已收 / 缺失（合并后）。
    pub fn status(&self) -> Result<(Vec<Range>, Vec<Range>, u64)> {
        let parts = self.scan_parts()?;
        let total = self.manifest.total_size;
        let mut received: Vec<Range> = Vec::new();
        let mut received_bytes = 0u64;
        for p in &parts {
            received_bytes += p.length;
            match received.last_mut() {
                Some(last) if last.offset + last.length == p.offset => last.length += p.length,
                _ => received.push(*p),
            }
        }
        let mut missing = Vec::new();
        let mut cursor = 0u64;
        for r in &received {
            if r.offset > cursor {
                missing.push(Range {
                    offset: cursor,
                    length: r.offset - cursor,
                });
            }
            cursor = cursor.max(r.offset + r.length);
        }
        if cursor < total {
            missing.push(Range {
                offset: cursor,
                length: total - cursor,
            });
        }
        Ok((received, missing, received_bytes))
    }

    /// 写一片。**调用方必须持锁**。
    ///
    /// 顺序：越界 → 重叠判定 → 摘要（碰磁盘之前）→ 独占临时文件 → 写 → fsync →
    /// rename → fsync(parts/)。
    pub fn write_part(
        &self,
        offset: u64,
        bytes: &[u8],
        declared_sha256: &str,
    ) -> std::result::Result<PartOutcome, ChunkError> {
        let length = bytes.len() as u64;
        if length == 0 {
            return Err(ChunkError::OutOfRange("分片不能为空".into()));
        }
        let end = offset
            .checked_add(length)
            .ok_or_else(|| ChunkError::OutOfRange("分片区间溢出".into()))?;
        let total = self.manifest.total_size;
        if end > total {
            return Err(ChunkError::OutOfRange(format!(
                "分片区间 [{offset}, {end}) 越过总大小 {total}"
            )));
        }
        // 网格：非末段的 offset 与 length 都要对齐 base_unit。
        let base = BASE_UNIT as u64;
        if offset % base != 0 || (end != total && length % base != 0) {
            return Err(ChunkError::NotAligned(format!(
                "分片 [{offset}, {end}) 未按 {base} 字节网格对齐"
            )));
        }

        let declared = declared_sha256.trim().to_ascii_lowercase();
        let actual = sha256_hex(bytes);
        if declared != actual {
            return Err(ChunkError::Digest);
        }

        let parts = self.scan_parts().map_err(ChunkError::Io)?;
        let target = self.parts_dir().join(format!("{offset}-{length}.part"));
        for p in &parts {
            let p_end = p.offset + p.length;
            let same = p.offset == offset && p.length == length;
            let overlaps = p.offset < end && offset < p_end;
            if same {
                // 幂等判据：流式重算已有 part 的摘要与请求头比对。
                let existing = sha256_of_file(&target).map_err(ChunkError::Io)?;
                if existing == actual {
                    return Ok(PartOutcome::AlreadyPresent);
                }
                return Err(ChunkError::Overlap(format!(
                    "分片 [{offset}, {end}) 已存在且内容不同"
                )));
            }
            if overlaps {
                return Err(ChunkError::Overlap(format!(
                    "分片 [{offset}, {end}) 与已收 [{}, {p_end}) 边界重叠", p.offset
                )));
            }
        }

        let tmp = self.parts_dir().join(format!(".{offset}-{length}.tmp"));
        let write = (|| -> std::io::Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            std::fs::rename(&tmp, &target)?;
            File::open(self.parts_dir())?.sync_all()
        })();
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(ChunkError::Io(ServerError::Internal(format!(
                "写分片失败: {e}"
            ))));
        }
        Ok(PartOutcome::Written)
    }

    /// 拼接：校验恰好覆盖 → 按序流式拼进 `body.complete.tmp` 边算 SHA-256 → 与 manifest
    /// 摘要比对 → fsync。**调用方必须持锁**。
    ///
    /// 返回 `(拼接文件路径, 字节数, 摘要)`。
    pub fn assemble(&self) -> std::result::Result<(PathBuf, u64, String), AssembleError> {
        let (received, missing, _) = self.status().map_err(AssembleError::Io)?;
        if !missing.is_empty() {
            return Err(AssembleError::Missing(missing));
        }
        // 无重叠：合并后应恰好是一段 [0, total)。
        let total = self.manifest.total_size;
        if received.len() != 1 || received[0].offset != 0 || received[0].length != total {
            return Err(AssembleError::Overlap);
        }
        let parts = self.scan_parts().map_err(AssembleError::Io)?;

        let out_path = self.assembled_path();
        let result = (|| -> std::io::Result<String> {
            let mut out = File::create(&out_path)?;
            let mut hasher = sha2::Sha256::new();
            let mut buf = vec![0u8; 1 << 20];
            for p in &parts {
                let mut f = File::open(self.parts_dir().join(format!("{}-{}.part", p.offset, p.length)))?;
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&buf[..n]);
                    out.write_all(&buf[..n])?;
                }
            }
            out.sync_all()?;
            Ok(hex::encode(hasher.finalize()))
        })();
        let sha = match result {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&out_path);
                return Err(AssembleError::Io(ServerError::Internal(format!(
                    "拼接分片失败: {e}"
                ))));
            }
        };
        if !sha.eq_ignore_ascii_case(&self.manifest.sealed_sha256) {
            let _ = std::fs::remove_file(&out_path);
            return Err(AssembleError::DigestMismatch);
        }
        Ok((out_path, total, sha))
    }

    /// 删除整个会话目录。**调用方必须持锁**（锁文件随目录一起消失，锁随句柄释放）。
    pub fn discard(&self) -> Result<()> {
        std::fs::remove_dir_all(&self.dir)
            .map_err(|e| ServerError::Internal(format!("删除会话目录 {:?} 失败: {e}", self.dir)))
    }
}

/// `PUT chunk` 的失败分类。
#[derive(Debug)]
pub enum ChunkError {
    OutOfRange(String),
    NotAligned(String),
    /// 内容与 `X-Chunk-SHA256` 不符（可重试）。
    Digest,
    /// 与已有 part 边界重叠但不完全相同 / 同边界不同内容。
    Overlap(String),
    Io(ServerError),
}

/// complete 拼接失败分类。
#[derive(Debug)]
pub enum AssembleError {
    Missing(Vec<Range>),
    Overlap,
    DigestMismatch,
    Io(ServerError),
}

/// 24 小时扫描：删掉所有**已过期**且**能拿到非阻塞锁**的会话目录。返回删了几个。
///
/// 🔴 按 transport 分流（第十五轮评审 P0）：S3 会话必须先完成 MPU abort /
/// final object 归属处置才能删目录，由 [`sweep_expired_s3`] 负责；这里直接跳过，
/// 否则目录一删，`provider_upload_id`/`final_key`/session_id 永久丢失，删除失败的
/// 对象连重试入口都没了。
pub fn sweep_expired(session_root: &Path) -> usize {
    let root = chunked_root(session_root);
    let Ok(rd) = std::fs::read_dir(&root) else { return 0 };
    let now = now_secs();
    let mut removed = 0;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest = read_json::<Manifest>(&dir.join("manifest.json"));
        let expired = match &manifest {
            Ok(Some(m)) => now > m.expires_at,
            // manifest 缺失/损坏：这个目录已经没人能用了，也清。
            _ => true,
        };
        if !expired {
            continue;
        }
        if matches!(&manifest, Ok(Some(m)) if m.transport == TRANSPORT_S3_MULTIPART_V1) {
            continue;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("session.lock"))
            .ok();
        let held = match lock.as_ref() {
            Some(f) => matches!(flock_nb(f), Ok(true)),
            // 没有锁文件 = 半建成的目录，直接清。
            None => true,
        };
        if !held {
            continue;
        }
        if std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// abort MPU 并用 `ListParts` 确认清空（RESUMABLE §8.7 判据 20，第十六轮评审 P1）：
/// abort 后确认，残留继续 abort，上限 3 轮。返回 `true` = 已确认清空。
async fn abort_until_confirmed(
    backend: &std::sync::Arc<dyn super::numbered_parts::NumberedPartBackend>,
    reference: &super::numbered_parts::UploadReference,
) -> bool {
    use super::numbered_parts::NumberedPartError;
    for _ in 0..3 {
        if let Err(e) = backend.abort(reference).await {
            if !matches!(e, NumberedPartError::NoSuchUpload) {
                tracing::warn!("扫描器 abort MPU 失败（保留，下一轮重试）: {e:?}");
                return false;
            }
        }
        match backend.list_parts(reference).await {
            // NoSuchUpload = MPU 已彻底关闭；空列表 = parts 已清。二者都算确认。
            Err(NumberedPartError::NoSuchUpload) => return true,
            Ok(parts) if parts.is_empty() => return true,
            Ok(parts) => {
                tracing::warn!("扫描器：abort 后 MPU 仍残留 {} 片，继续 abort", parts.len());
            }
            Err(e) => {
                tracing::warn!("扫描器：ListParts 确认失败（保留，下一轮重试）: {e:?}");
                return false;
            }
        }
    }
    tracing::warn!("扫描器：反复 abort 后 MPU 仍有残留分片，保留，下一轮重试");
    false
}

/// 锚点 GC（第十七轮评审 P1）：锚点是「MPU 已建但 manifest 不可读」的恢复记录。
/// - 对应会话目录健在（manifest 可读）→ 签发成功后的残留锚点，直接删；
/// - 否则孤儿：abort + ListParts 确认 → HEAD：无对象 → 删锚点并收掉对应目录（必须在这里收：锚点删后
///   主循环无法区分「刚恢复完」与「从未建过」）；对象在（无论归属）或任何一步失败 → 保留锚点（下一轮重试/人工）。🔴 manifest 不可读 ⇒ 无法查
///   `reserved_file_id` ⇒ 即使对象属于本会话也不能证明「无 PG 行」，绝不删对象。
async fn sweep_s3_anchors(
    session_root: &Path,
    backend: Option<&std::sync::Arc<dyn super::numbered_parts::NumberedPartBackend>>,
    probe: Option<&std::sync::Arc<dyn super::final_object_probe::FinalObjectProbe>>,
) {
    use super::numbered_parts::UploadReference;
    let anchor_root = s3_anchor_root(session_root);
    let Ok(rd) = std::fs::read_dir(&anchor_root) else { return };
    let chunked = chunked_root(session_root);
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(upload_id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        let anchor = match read_json::<S3Anchor>(&path) {
            Ok(Some(a)) => a,
            // 锚点文件损坏：没有可恢复的引用，同「无锚点」口径删掉。
            _ => {
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        // manifest 可读 → 签发成功后的残留（删锚失败留下的）：manifest 已接管。
        if matches!(
            read_json::<Manifest>(&chunked.join(&upload_id).join("manifest.json")),
            Ok(Some(_))
        ) {
            let _ = remove_s3_anchor(session_root, &upload_id);
            continue;
        }
        let (Some(backend), Some(probe)) = (backend, probe) else {
            tracing::error!(
                "扫描器：存在孤儿锚 {upload_id} 但直传门禁未接入（后端/探测缺失），保留"
            );
            continue;
        };
        let reference = UploadReference {
            bucket: anchor.bucket,
            final_key: anchor.final_key,
            provider_upload_id: anchor.provider_upload_id,
        };
        if !abort_until_confirmed(backend, &reference).await {
            continue;
        }
        match probe.head(&reference).await {
            Ok(None) => {
                // MPU 已关、对象不在：孤儿已清干净。收锚点与对应目录（此刻能证明的
                // 仅此而已；不先收目录，锚点删后主循环无法区分「刚恢复完」与「从未建过」）。
                let _ = remove_s3_anchor(session_root, &upload_id);
                let _ = std::fs::remove_dir_all(chunked.join(&upload_id));
            }
            Ok(Some(head)) => {
                let own = head.privchat_upload_id.as_deref() == Some(upload_id.as_str());
                tracing::error!(
                    "扫描器：孤儿锚 {upload_id} 的 final 对象仍在（属于本会话: {own}），无 manifest 不能证明可删，锚点与目录保留，人工排查"
                );
            }
            Err(e) => {
                tracing::warn!("扫描器：孤儿锚 {upload_id} HEAD 失败（保留，下一轮重试）: {e}");
            }
        }
    }
}

/// 已过期 S3 会话删目录前，对象侧必须先处置干净（RESUMABLE §8.7 判据 20）。
/// 返回 `Ok(true)` = 可以删目录；`Ok(false)` = 保留目录下一轮再试；`Err(())` =
/// manifest 半建（从未建过 MPU，无恢复信息），等同可删。
async fn s3_expired_session_ready(
    manifest: &Manifest,
    session_id: &str,
    backend: &std::sync::Arc<dyn super::numbered_parts::NumberedPartBackend>,
    probe: &std::sync::Arc<dyn super::final_object_probe::FinalObjectProbe>,
    file_service: &super::FileService,
) -> std::result::Result<bool, ()> {
    use super::numbered_parts::UploadReference;

    let reference = match (
        manifest.bucket.as_deref(),
        manifest.final_key.as_deref(),
        manifest.provider_upload_id.as_deref(),
    ) {
        (Some(bucket), Some(final_key), Some(pid)) => UploadReference {
            bucket: bucket.to_string(),
            final_key: final_key.to_string(),
            provider_upload_id: pid.to_string(),
        },
        // 半建会话：MPU 从未创建，没有对象侧负担。
        _ => return Err(()),
    };

    // 1. abort MPU 并确认清空（公共口径，见 [`abort_until_confirmed`]）。
    if !abort_until_confirmed(backend, &reference).await {
        return Ok(false);
    }

    // 2. HEAD final_key。
    let head = match probe.head(&reference).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("扫描器 HEAD final key 失败（目录保留，下一轮重试）: {e}");
            return Ok(false);
        }
    };
    let Some(head) = head else {
        // 对象不存在：MPU 已 abort，无残留。
        return Ok(true);
    };

    // 3. 归属：metadata 不属于本会话 → 永不删对象；目录保留作人工排查锚点。
    if head.privchat_upload_id.as_deref() != Some(session_id) {
        tracing::error!(
            "扫描器：final key 上对象不属于过期会话 {session_id}，保留对象与目录，人工排查"
        );
        return Ok(false);
    }

    // 4. 属于本会话：先查 PG——「PG 已提交、墓碑没写」的崩溃窗口里对象是
    // 正式数据，绝不能删；只有无行引用的对象才是冗余，才条件删除。
    match file_service.get_file_metadata(manifest.reserved_file_id).await {
        Ok(Some(meta))
            if meta.uploader_id == manifest.uploader_id
                && meta.file_size == manifest.total_size
                && meta
                    .file_hash
                    .as_deref()
                    .is_some_and(|h| h.eq_ignore_ascii_case(&manifest.sealed_sha256)) =>
        {
            tracing::info!(
                "扫描器：过期会话 {session_id} 的 final 对象已有 PG 行（file_id={}），保留对象，只删目录",
                manifest.reserved_file_id
            );
            return Ok(true);
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("扫描器：查 reserved_file_id 失败（目录保留，下一轮重试）: {e}");
            return Ok(false);
        }
    }

    // 5. 无行引用 → 归属已证明（统一删除规则），条件删除（ETag 防 TOCTOU）。
    match probe.delete_if_match(&reference, &head.etag).await {
        Ok(true) => Ok(true),
        Ok(false) => {
            tracing::warn!("扫描器：删除前 final 对象已变化（ETag 不匹配），目录保留，下一轮重新核验");
            Ok(false)
        }
        Err(e) => {
            tracing::warn!("扫描器：删除 final 对象失败（目录保留，下一轮重试）: {e}");
            Ok(false)
        }
    }
}

/// 24 小时扫描的 S3 分支（RESUMABLE §8.7 判据 20，第十五轮评审 P0）：
/// 🔴 过期且无墓碑的 S3 会话，必须先持非阻塞锁完成 abort / HEAD / 归属核验 /
/// 恢复或条件删除，**成功之后才删目录**；任何一步失败都保留目录下一轮再试，
/// 绝不先丢 `provider_upload_id`/`final_key`/session_id 这些恢复信息。
/// 桶 lifecycle `AbortIncompleteMultipartUpload` 仅兜底目录整体丢失的场景。
pub async fn sweep_expired_s3(
    session_root: &Path,
    backend: Option<&std::sync::Arc<dyn super::numbered_parts::NumberedPartBackend>>,
    probe: Option<&std::sync::Arc<dyn super::final_object_probe::FinalObjectProbe>>,
    file_service: &super::FileService,
) -> usize {
    // 锚点 GC 先行（第十七轮评审 P1）：清掉「MPU 已建但 manifest 不可读」的孤儿，
    // 同时把签发成功后的残留锚点收掉；主循环的损坏分支据此判断可否删目录。
    sweep_s3_anchors(session_root, backend, probe).await;

    let root = chunked_root(session_root);
    let Ok(rd) = std::fs::read_dir(&root) else { return 0 };
    let now = now_secs();
    let mut removed = 0;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest = match read_json::<Manifest>(&dir.join("manifest.json")) {
            Ok(Some(m)) => m,
            // manifest 缺失/损坏（第十七轮评审 P2）：不盲删。有锚点 → MPU 已建，
            // 目录保留（对象侧已由锚点 GC 处置）；无锚点 → 可证 MPU 从未创建
            // （不变式：create 成功必先写锚），与 proxy 同口径直接清。
            _ => {
                let upload_id = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if matches!(read_s3_anchor(session_root, &upload_id), Ok(Some(_))) {
                    tracing::warn!(
                        "扫描器：会话 {upload_id} manifest 不可读但存在 S3 锚点，目录保留，等锚点 GC 恢复"
                    );
                    continue;
                }
                if std::fs::remove_dir_all(&dir).is_ok() {
                    removed += 1;
                }
                continue;
            }
        };
        if now <= manifest.expires_at || manifest.transport != TRANSPORT_S3_MULTIPART_V1 {
            continue;
        }
        // 非阻塞锁：拿不到说明有 in-flight 请求，本轮跳过。🔴 墓碑分支同样在锁后：
        // complete 写完墓碑可能仍持锁未返回，锁前删目录会删掉进行中的完成流程
        // （第十六轮评审 P0）。
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.join("session.lock"))
            .ok();
        let held = match lock.as_ref() {
            Some(f) => matches!(flock_nb(f), Ok(true)),
            None => true,
        };
        if !held {
            continue;
        }
        // 墓碑在：complete 已终态（MPU 已终结、对象已处置），与 proxy 同口径。
        if dir.join("completed.json").exists() {
            if std::fs::remove_dir_all(&dir).is_ok() {
                removed += 1;
                // 锚点随目录生命周期终结（正常早已删，这里兜底）。
                let _ = remove_s3_anchor(session_root, &session_id_at(&dir));
            }
            continue;
        }
        let session_id = dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let (Some(backend), Some(probe)) = (backend, probe) else {
            tracing::error!(
                "扫描器：存在过期 S3 会话 {session_id} 但直传门禁未接入（后端/探测缺失），目录保留"
            );
            continue;
        };
        let ready = match s3_expired_session_ready(&manifest, &session_id, backend, probe, file_service).await {
            Ok(ready) => ready,
            // 半建会话（MPU 从未创建）：无对象侧负担，可删。
            Err(()) => true,
        };
        if ready && std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
            let _ = remove_s3_anchor(session_root, &session_id);
        }
    }
    removed
}

fn session_id_at(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn sha256_of_file(path: &Path) -> Result<String> {
    let mut f = File::open(path)
        .map_err(|e| ServerError::Internal(format!("打开 {path:?} 失败: {e}")))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| ServerError::Internal(format!("读 {path:?} 失败: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(unix)]
fn flock_nb(file: &File) -> Result<bool> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: fd 来自一个活着的 File，flock 不会转移所有权。
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(libc::EWOULDBLOCK) | Some(libc::EINTR)) {
        return Ok(false);
    }
    Err(ServerError::Internal(format!("flock 失败: {err}")))
}

#[cfg(not(unix))]
fn flock_nb(_file: &File) -> Result<bool> {
    Err(ServerError::Internal("上传会话锁仅支持 Unix".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_input(total: u64, sha: &str) -> NewSession {
        NewSession {
            uploader_id: 7,
            total_size: total,
            sealed_sha256: sha.to_string(),
            file_type: "image".into(),
            business_type: "message".into(),
            filename: "payload.jpg".into(),
            mime_type: "image/jpeg".into(),
            transform_version: 0,
            reserved_file_id: 4242,
            transport: TRANSPORT_PROXY_OFFSET_V1.to_string(),
            s3: None,
        }
    }

    fn make(total: usize) -> (tempfile::TempDir, ChunkedSession, String, Vec<u8>) {
        let root = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..total).map(|i| (i % 251) as u8).collect();
        let sha = sha256_hex(&data);
        let (s, token, _) = ChunkedSession::create(root.path(), new_input(total as u64, &sha)).unwrap();
        (root, s, token, data)
    }

    #[test]
    fn token_roundtrip_and_bad_secret_is_rejected() {
        let (root, s, token, _) = make(10);
        let again = ChunkedSession::open(root.path(), &token).unwrap();
        assert_eq!(again.upload_id(), s.upload_id());
        let (id, _) = parse_token(&token).unwrap();
        let forged = format!("{id}.{}", "0".repeat(SECRET_HEX_LEN));
        assert!(matches!(ChunkedSession::open(root.path(), &forged), Err(OpenError::BadSecret)));
        assert!(matches!(ChunkedSession::open(root.path(), "junk"), Err(OpenError::Malformed)));
    }

    #[test]
    fn out_of_order_parts_assemble_and_verify() {
        let unit = BASE_UNIT as usize;
        let total = unit * 2 + 100;
        let (_root, s, _, data) = make(total);
        let _l = s.try_lock().unwrap().unwrap();
        // 末段先到，再中段，再首段。
        for (off, len) in [(unit * 2, 100), (unit, unit), (0, unit)] {
            let bytes = &data[off..off + len];
            let r = s.write_part(off as u64, bytes, &sha256_hex(bytes)).unwrap();
            assert_eq!(r, PartOutcome::Written);
        }
        let (recv, missing, n) = s.status().unwrap();
        assert!(missing.is_empty());
        assert_eq!(n, total as u64);
        assert_eq!(recv, vec![Range { offset: 0, length: total as u64 }]);
        let (path, written, sha) = s.assemble().unwrap();
        assert_eq!(written, total as u64);
        assert_eq!(sha, sha256_hex(&data));
        assert_eq!(std::fs::read(path).unwrap(), data);
    }

    #[test]
    fn same_part_twice_is_idempotent_and_different_content_conflicts() {
        let unit = BASE_UNIT as usize;
        let (_root, s, _, data) = make(unit * 2);
        let _l = s.try_lock().unwrap().unwrap();
        let bytes = &data[0..unit];
        let d = sha256_hex(bytes);
        assert_eq!(s.write_part(0, bytes, &d).unwrap(), PartOutcome::Written);
        assert_eq!(s.write_part(0, bytes, &d).unwrap(), PartOutcome::AlreadyPresent);
        let other = vec![9u8; unit];
        assert!(matches!(
            s.write_part(0, &other, &sha256_hex(&other)),
            Err(ChunkError::Overlap(_))
        ));
        // 边界重叠但不相同。
        let half = &data[0..unit / 2];
        assert!(matches!(
            s.write_part(0, half, &sha256_hex(half)),
            Err(ChunkError::Overlap(_)) | Err(ChunkError::NotAligned(_))
        ));
    }

    #[test]
    fn digest_mismatch_never_touches_disk() {
        let unit = BASE_UNIT as usize;
        let (_root, s, _, data) = make(unit);
        let _l = s.try_lock().unwrap().unwrap();
        assert!(matches!(
            s.write_part(0, &data, &"0".repeat(64)),
            Err(ChunkError::Digest)
        ));
        assert!(s.scan_parts().unwrap().is_empty());
        assert_eq!(std::fs::read_dir(s.dir().join("parts")).unwrap().count(), 0);
    }

    #[test]
    fn status_reports_missing_ranges() {
        let unit = BASE_UNIT as u64;
        let (_root, s, _, data) = make(unit as usize * 3);
        let _l = s.try_lock().unwrap().unwrap();
        let mid = &data[unit as usize..unit as usize * 2];
        s.write_part(unit, mid, &sha256_hex(mid)).unwrap();
        let (_, missing, n) = s.status().unwrap();
        assert_eq!(n, unit);
        assert_eq!(
            missing,
            vec![Range { offset: 0, length: unit }, Range { offset: unit * 2, length: unit }]
        );
        assert!(matches!(s.assemble(), Err(AssembleError::Missing(_))));
    }

    #[test]
    fn tombstone_survives_dropping_the_payload() {
        let (root, s, token, _) = make(10);
        s.write_completed(99).unwrap();
        s.drop_payload();
        let again = ChunkedSession::open(root.path(), &token).unwrap();
        assert_eq!(again.completed_file_id().unwrap(), Some(99));
        assert!(!again.dir().join("parts").exists());
    }

    #[test]
    fn expired_sessions_are_swept_but_live_ones_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let (live, _, _) = ChunkedSession::create(root.path(), new_input(1, "aa")).unwrap();
        let (dead, _, _) = ChunkedSession::create(root.path(), new_input(1, "bb")).unwrap();
        let mut m = dead.manifest().clone();
        m.expires_at = 1;
        write_json_atomic(dead.dir(), "manifest.json", &m).unwrap();
        assert_eq!(sweep_expired(root.path()), 1);
        assert!(live.dir().exists());
        assert!(!dead.dir().exists());
    }

    #[test]
    fn a_held_lock_blocks_the_sweeper_and_a_second_locker() {
        let root = tempfile::tempdir().unwrap();
        let (s, _, _) = ChunkedSession::create(root.path(), new_input(1, "aa")).unwrap();
        let mut m = s.manifest().clone();
        m.expires_at = 1;
        write_json_atomic(s.dir(), "manifest.json", &m).unwrap();
        let held = s.try_lock().unwrap().unwrap();
        assert!(s.try_lock().unwrap().is_none());
        assert_eq!(sweep_expired(root.path()), 0);
        drop(held);
        assert_eq!(sweep_expired(root.path()), 1);
    }

    /// §8.2 单一数据面（第二十轮）：配置单选、禁回退；配了 S3 只认 S3 声明。
    #[test]
    fn transport_selection_follows_single_data_plane() {
        let closed = S3DirectGate { open: false };
        let open = S3DirectGate { open: true };
        // proxy 数据面：旧客户端隐式 proxy；声明含 proxy → proxy（文件大小无关）。
        assert_eq!(select_transport(None, &closed), Ok(TRANSPORT_PROXY_OFFSET_V1));
        assert_eq!(select_transport(None, &open), Err(TransportSelectError::ServerS3Only));
        let only_proxy = vec![TRANSPORT_PROXY_OFFSET_V1.to_string()];
        assert_eq!(
            select_transport(Some(&only_proxy), &closed),
            Ok(TRANSPORT_PROXY_OFFSET_V1)
        );
        // proxy 数据面下声明了 S3 也不算非法（含 proxy 即可）→ proxy，无 S3 可发。
        let with_s3 = vec![
            TRANSPORT_PROXY_OFFSET_V1.to_string(),
            TRANSPORT_S3_MULTIPART_V1.to_string(),
        ];
        assert_eq!(
            select_transport(Some(&with_s3), &closed),
            Ok(TRANSPORT_PROXY_OFFSET_V1)
        );
        // 🔴 S3 单一数据面：声明了 S3 → S3（任何文件大小，无阈值）；
        // 未声明（含旧客户端、只声明 proxy）→ 报错，绝不回退。
        assert_eq!(
            select_transport(Some(&with_s3), &open),
            Ok(TRANSPORT_S3_MULTIPART_V1)
        );
        assert_eq!(
            select_transport(Some(&[TRANSPORT_S3_MULTIPART_V1.to_string()]), &open),
            Ok(TRANSPORT_S3_MULTIPART_V1)
        );
        assert_eq!(
            select_transport(Some(&only_proxy), &open),
            Err(TransportSelectError::ServerS3Only)
        );
        // proxy 数据面的集合规则不变：不含 proxy_offset_v1 → Err（空集合、只声明
        // S3、未知 transport），与门禁状态无关。
        assert_eq!(
            select_transport(Some(&[TRANSPORT_S3_MULTIPART_V1.to_string()]), &closed),
            Err(TransportSelectError::SetMissingProxy)
        );
        assert_eq!(select_transport(Some(&[]), &closed), Err(TransportSelectError::SetMissingProxy));
        assert_eq!(
            select_transport(Some(&["weird".to_string()]), &closed),
            Err(TransportSelectError::SetMissingProxy)
        );
    }

    /// §8.1 冻结分片几何：默认 8 MiB、按 10000 片上限抬升、1 MiB 对齐、
    /// 限域 [5 MiB, 5 GiB]。
    #[test]
    fn s3_part_geometry_follows_the_frozen_formula() {
        // 小文件：默认 8 MiB，单片。
        assert_eq!(s3_part_geometry(16 << 20), (8 << 20, 2));
        assert_eq!(s3_part_geometry(1 << 20), (8 << 20, 1));
        // 非整片：末片余数由 check_part_geometry 管，这里只出片数。
        assert_eq!(s3_part_geometry((10 << 20) + 1), (8 << 20, 2));
        // 100 GiB：ceil(100GiB/10000)=10.48576 MiB → 对齐 11 MiB。
        let (ps, n) = s3_part_geometry(100 << 30);
        assert_eq!(ps, 11 << 20);
        assert_eq!(n as u64, (100u64 << 30).div_ceil(11 << 20));
        // 极大文件：片大小抬到上限 5 GiB。
        let (ps, n) = s3_part_geometry(50_000u64 << 30);
        assert_eq!(ps, 5 << 30);
        assert_eq!(n, 10_000);
        // 总片数永远 ≤ 10000。
        for size in [1u64, 1 << 30, 80 << 30, 5 << 40] {
            let (_, n) = s3_part_geometry(size);
            assert!(n <= 10_000, "size={size} 片数 {n} 超限");
        }
    }
}
