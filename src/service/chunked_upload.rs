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
//!   manifest.json        # 冻结事实（申请 token 时一次写成，之后只读）
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

/// 协商失败：声明的能力集合缺少 `proxy_offset_v1`（回退保底，RESUMABLE §8.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportSetMissingProxy;

/// 协商与模式选择（RESUMABLE_UPLOAD_SPEC §8.2，纯加法）。
///
/// 🔴 集合规则由本函数自己强制，不靠调用方注释维持：`declared` 为 `Some` 且不含
/// `proxy_offset_v1` → `Err(TransportSetMissingProxy)`（RPC 层映射为参数错误）。
/// `declared == None`（旧客户端）→ 隐式 proxy，行为逐字节不变。服务端永远不会
/// 返回客户端声明集合之外的 transport。
/// 当前 S3 门禁（`direct_upload` 显式配置 + `s3_direct_threshold` + 集成门禁）尚未
/// 接入，即使客户端声明了 `s3_multipart_v1` 也回退 proxy；分支结构留在原地，供后续
/// 步骤接入，不得在别的处另写判定。
pub fn select_transport(
    declared: Option<&[String]>,
    _file_size: u64,
) -> std::result::Result<&'static str, TransportSetMissingProxy> {
    match declared {
        // 字段省略：旧客户端，隐式 proxy；现有自适应分片逻辑完全不变。
        None => Ok(TRANSPORT_PROXY_OFFSET_V1),
        // 🔴 集合规则：不含 proxy_offset_v1 → 拒绝，调用方回参数错误。
        Some(list) if !list.iter().any(|t| t == TRANSPORT_PROXY_OFFSET_V1) => {
            Err(TransportSetMissingProxy)
        }
        // 未声明 s3_multipart_v1 → proxy。
        Some(list) if !list.iter().any(|t| t == TRANSPORT_S3_MULTIPART_V1) => {
            Ok(TRANSPORT_PROXY_OFFSET_V1)
        }
        // 声明支持 S3 → 还要过门禁：file_size >= s3_direct_threshold + 存储源显式
        // direct_upload 配置 + 集成门禁（RESUMABLE §8.2）；不满足时回退 proxy（合法，
        // 因为集合必含 proxy_offset_v1）。门禁未接入前恒 proxy。
        Some(_) => Ok(TRANSPORT_PROXY_OFFSET_V1),
    }
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
    #[serde(default)]
    pub created_at: i64,
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
    /// 协商选定的数据面（RESUMABLE §8.2），当前恒 `proxy_offset_v1`。
    pub transport: String,
}

impl ChunkedSession {
    /// 建目录 + 写 manifest，**成功之后才**返回 token。
    ///
    /// 返回 `(session, token, expires_at)`。
    pub fn create(session_root: &Path, input: NewSession) -> Result<(Self, String, i64)> {
        let root = chunked_root(session_root);
        std::fs::create_dir_all(&root)
            .map_err(|e| ServerError::Internal(format!("创建 {root:?} 失败: {e}")))?;

        let upload_id = hex::encode(rand::random::<[u8; 16]>());
        let secret = hex::encode(rand::random::<[u8; 32]>());
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
            // S3 分片参数在直传门禁接入（实现顺序第 5 步）建 S3 会话时才写入。
            part_size: None,
            total_parts: None,
            bucket: None,
            final_key: None,
            provider_upload_id: None,
            created_at: now,
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
        let expired = match read_json::<Manifest>(&dir.join("manifest.json")) {
            Ok(Some(m)) => now > m.expires_at,
            // manifest 缺失/损坏：这个目录已经没人能用了，也清。
            _ => true,
        };
        if !expired {
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

    /// §8.2 协商：门禁未接入前所有分支恒 proxy_offset_v1；旧客户端（None）
    /// 与新客户端的行为差异只体现在响应字段，不体现在模式选择。
    #[test]
    fn transport_selection_is_proxy_only_until_gates_land() {
        assert_eq!(select_transport(None, 1), Ok(TRANSPORT_PROXY_OFFSET_V1));
        assert_eq!(select_transport(None, 1 << 30), Ok(TRANSPORT_PROXY_OFFSET_V1));
        let only_proxy = vec![TRANSPORT_PROXY_OFFSET_V1.to_string()];
        assert_eq!(select_transport(Some(&only_proxy), 1 << 30), Ok(TRANSPORT_PROXY_OFFSET_V1));
        let with_s3 = vec![
            TRANSPORT_PROXY_OFFSET_V1.to_string(),
            TRANSPORT_S3_MULTIPART_V1.to_string(),
        ];
        // 声明了 s3_multipart_v1 也回退 proxy：direct_upload 配置 + 阈值 + 集成门禁
        // 均未接入（实现顺序第 5 步），接入前不得提前放行。回退合法：集合含 proxy。
        assert_eq!(select_transport(Some(&with_s3), 1 << 30), Ok(TRANSPORT_PROXY_OFFSET_V1));
        // 🔴 集合规则由本函数自身强制：不含 proxy_offset_v1 → Err（含空集合、
        // 只声明 S3、未知 transport），不依赖调用方先校验。
        assert_eq!(
            select_transport(Some(&[TRANSPORT_S3_MULTIPART_V1.to_string()]), 1 << 30),
            Err(TransportSetMissingProxy)
        );
        assert_eq!(select_transport(Some(&[]), 1 << 30), Err(TransportSetMissingProxy));
        assert_eq!(
            select_transport(Some(&["weird".to_string()]), 1 << 30),
            Err(TransportSetMissingProxy)
        );
    }
}
