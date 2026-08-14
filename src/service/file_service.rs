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

//! 文件服务 - 处理文件上传、存储和管理
//!
//! 基于 [OpenDAL](https://opendal.apache.org/) 统一对象存储抽象：本地 FS 与 S3/OSS/COS/MinIO/Garage 等
//! 共用同一套 Operator API（write/read/delete），实现轻量、通用。
//!
//! 上传服务只负责存储，不做压缩/缩略图；类型、大小、业务等以请求上传 token 时的约定为准。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use opendal::Operator;

use crate::config::FileStorageSourceConfig;
use crate::error::{Result, ServerError};
use crate::repository::FileUploadRepository;

// 向后兼容：从 service 层继续导出类型（upload_token_service 等使用）
pub use crate::model::file_upload::{FileMetadata, FileType};

/// 存储源 ID：0=本地，1=S3 等
pub const STORAGE_SOURCE_LOCAL: u32 = 0;

/// 文件 URL 响应（用于 get_file_url）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUrlResponse {
    pub file_url: String,
    pub thumbnail_url: Option<String>,
    pub expires_at: i64,
    pub file_size: u64,
    pub mime_type: String,
    pub storage_source_id: u32,
    /// 附件加密版本：0=明文；1=AES-256-GCM。
    pub encryption_version: i32,
    /// CEK（base64url 32B）；仅鉴权后返回，绝不进日志/URL。version=0 时 None。
    pub cek: Option<String>,
}

/// 一个文件的引用现状：由调用方把 IO 查好再传进来（纯决策，便于单测）。
///
/// 三个字段各自回答一个独立问题，**不能互相推导**：
/// - `has_any_reference`：这个文件有没有被任何消息引用过（含已撤回/已删除的）。
///   `false` = pending（上传了还没发出去）。
/// - `requester_is_member_of_a_live_reference`：请求者是不是**某条仍然有效**的引用
///   消息所在会话的成员。这是放行的正条件。
/// - `uploader_id` / `requester_id`：pending 阶段唯一的判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAccessFacts {
    pub requester_id: u64,
    pub uploader_id: u64,
    pub has_any_reference: bool,
    pub requester_is_member_of_a_live_reference: bool,
}

/// 附件访问授权纯决策（无 IO；MEDIA_REFERENCE_AND_FORWARD_SPEC §4.1）。
///
/// ```text
/// 放行 ⟺ 存在一条引用该文件的消息 M，且
///          M.deleted = false AND M.revoked = false
///          AND requester 是 M.channel_id 的成员
///      或  文件尚未被任何消息引用（pending）AND requester == uploader
/// ```
///
/// 🔴 授权主体是 **requester**，不是 sender。写成 sender 会变成
/// 「A 发的消息，B 下载时按 A 的权限放行」。
///
/// 🔴 uploader 身份**不能**绕过成员校验：文件一旦被消息引用，就只看会话成员关系。
/// 否则上传者可以下载自己被转发到陌生群里的那份，反过来也给了「先上传再蹭权限」的口子。
///
/// 🔴 「有引用但全都失效」≠「没有引用」。前者拒绝（撤回后不该再能下载），
/// 后者回落 uploader（还没发出去，只有自己能看）。
pub fn authorize_file_access(facts: FileAccessFacts) -> bool {
    if facts.has_any_reference {
        facts.requester_is_member_of_a_live_reference
    } else {
        facts.requester_id == facts.uploader_id
    }
}

/// 文件服务（多存储源，按 default_storage_source_id 选择；存储层统一用 OpenDAL Operator）
pub struct FileService {
    sources_by_id: HashMap<u32, FileStorageSourceConfig>,
    /// 每个存储源对应一个 OpenDAL Operator（在 init 中按配置构建）
    operators: Arc<RwLock<HashMap<u32, Operator>>>,
    default_storage_source_id: u32,
    file_upload_repo: Arc<FileUploadRepository>,
}

/// 把校验通过的临时对象发布到正式路径。
///
/// 🔴 **no-clobber**：正式路径已存在时**绝不覆盖**。它可能是上一次「已发布但 PG
/// 未提交」留下的对象，也可能正被某条已提交记录引用着。覆盖 = 拿新字节顶掉别人
/// 正在引用的文件。已存在时交由调用方核验（大小 + 摘要）后决定继续还是报冲突。
///
/// 本地存储用 `link` + `unlink`：`rename` 会**静默覆盖**，而 `link` 在目标已存在时
/// 返回 `EEXIST`——no-clobber 由内核保证，不是靠「先 stat 再动手」那种有竞态的写法。
///
/// 🔴 **没有任何降级到「先 stat 再 copy」的分支。** 那种写法有两处致命问题：
/// stat 与 copy 之间的窗口里别人可以发布同一个路径（TOCTOU），而 `copy` 本身是**覆盖**
/// 语义——一次失败的 link 就足以把 no-clobber 整条保证绕过去。三条出路各归各的：
///   · `EEXIST` → 目标已存在，交给调用方核验（大小 + 摘要）；
///   · `EXDEV` → 走 [`publish_across_filesystems`]：复制到**目标盘内**的中转文件、
///     fsync、再在该盘内 link（依然 no-clobber）。临时目录与存储根同盘只是当前部署
///     的偶然，上传盘单独挂出来是常规操作，这条路不能没有（spec §9.2）；
///   · 其它错误 → 报错，不猜。
///
/// 非本地后端走**条件写**（`if_not_exists`），由后端保证「目标已存在就失败」；
/// 后端不具备这个能力时报错，而不是假装发布成功。
async fn publish_object(
    op: &Operator,
    local_root: Option<&str>,
    staging: &str,
    final_path: &str,
) -> Result<PublishOutcome> {
    if let Some(root) = local_root {
        let from = std::path::Path::new(root).join(staging);
        let to = std::path::Path::new(root).join(final_path);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ServerError::Internal(format!("创建目标目录失败: {e}")))?;
        }

        // 🔴 **字节先落盘，目录项后指过去。**
        //
        // `hard_link` 只改目录项，不保证内容已经在盘上。少了这一步，掉电后完全可能
        // 出现「PG 里有记录、正式路径上的文件是个空洞」——而记录一旦提交就是永久的。
        fsync_file(&from)?;

        return match std::fs::hard_link(&from, &to) {
            Ok(()) => {
                // 新目录项本身也要落盘，否则掉电后记录指向一个不存在的名字。
                fsync_dir(to.parent())?;
                // 发布成功后**立即**移除临时对象：客户端可能在这之后就离线了，
                // 清理不能只靠 callback。
                // 这次 unlink 不 fsync：最坏情况是留下一个临时文件，由扫描回收——
                // 这个方向的错误是可回收的垃圾，而不是丢数据。
                let _ = std::fs::remove_file(&from);
                Ok(PublishOutcome::Published)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                Ok(PublishOutcome::AlreadyPresent)
            }
            // 🔴 跨文件系统（`EXDEV`）：**必须**有降级路径，不能报错了事。
            //
            // 会话临时目录与存储根今天同盘只是当前部署的偶然；文件长大之后把上传盘
            // 单独挂出来是常规操作，那一刻所有上传都会失败。spec §9.2 明确要求：
            // 流式复制到**目标文件系统内**的临时名 → fsync → 在该文件系统内原子发布。
            //
            // 📌 spec 原文写的是「原子 rename」，这里用 link + unlink：rename 会静默
            // 覆盖，与 no-clobber 冲突；link 同样原子，且目标存在时返回 EEXIST。

            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                publish_across_filesystems(&from, &to)
            }
            Err(e) => Err(ServerError::Internal(format!(
                "发布对象失败（{from:?} → {to:?}）：{e}"
            ))),
        };
    }

    // 非本地后端：no-clobber 只能由后端的条件写提供。
    if !op.info().full_capability().write_with_if_not_exists {
        return Err(ServerError::Internal(
            "该存储后端不支持条件写（if_not_exists），无法保证不覆盖已有对象，拒绝发布"
                .to_string(),
        ));
    }
    let total = op
        .stat(staging)
        .await
        .map_err(|e| ServerError::Internal(format!("读临时对象大小失败: {e}")))?
        .content_length();
    let reader = op
        .reader(staging)
        .await
        .map_err(|e| ServerError::Internal(format!("打开临时对象失败: {e}")))?;
    // 🔴 条件不满足可能在**两个**时刻报出来：后端要么在打开 writer 时就拒（本地文件
    // 系统的 `O_EXCL` 是立刻知道的），要么等到收尾提交时才拒（S3 的 `If-None-Match`
    // 跟着最后那个请求走）。两处都必须认成「已存在」，漏掉任何一处，一次正常的并发
    // 发布就会变成 500。
    let mut writer = match op.writer_with(final_path).if_not_exists(true).await {
        Ok(w) => w,
        Err(e) if e.kind() == opendal::ErrorKind::ConditionNotMatch => {
            return Ok(PublishOutcome::AlreadyPresent);
        }
        Err(e) => return Err(ServerError::Internal(format!("打开发布 writer 失败: {e}"))),
    };
    let mut offset = 0u64;
    while offset < total {
        let end = (offset + VERIFY_CHUNK).min(total);
        let buf = reader
            .read(offset..end)
            .await
            .map_err(|e| ServerError::Internal(format!("读临时对象失败: {e}")))?;
        writer
            .write(buf)
            .await
            .map_err(|e| ServerError::Internal(format!("写正式对象失败: {e}")))?;
        offset = end;
    }
    match writer.close().await {
        Ok(_) => {
            let _ = op.delete(staging).await;
            Ok(PublishOutcome::Published)
        }
        Err(e) if e.kind() == opendal::ErrorKind::ConditionNotMatch => {
            Ok(PublishOutcome::AlreadyPresent)
        }
        Err(e) => Err(ServerError::Internal(format!("发布对象失败: {e}"))),
    }
}

/// 跨文件系统发布：复制到**目标盘内**的临时名 → fsync → 在该盘内原子发布。
///
/// 🔴 临时名必须落在**目标目录**里，不能落在源盘：跨盘的 link 一样会 `EXDEV`，
/// 绕了一圈还是发布不出去。
///
/// 🔴 中转文件必须**独占创建**（`create_new`），不能只靠名字「大概不会重」。
/// 见 [`create_exclusive_temp`]。
fn publish_across_filesystems(from: &std::path::Path, to: &std::path::Path) -> Result<PublishOutcome> {
    let dir = to.parent().ok_or_else(|| {
        ServerError::Internal(format!("正式路径 {to:?} 没有父目录"))
    })?;
    let (mut dst, tmp) = create_exclusive_temp(dir)?;

    // 复制本身是流式的（`io::copy` 走固定大小缓冲），200MB 的文件不会进内存。
    let copy = (|| -> std::io::Result<()> {
        let mut src = std::fs::File::open(from)?;
        std::io::copy(&mut src, &mut dst)?;
        // 先让内容落盘，再让目录项指过去——顺序和同盘那条路径是一样的。
        dst.sync_all()
    })();
    if let Err(e) = copy {
        let _ = std::fs::remove_file(&tmp);
        return Err(ServerError::Internal(format!(
            "跨文件系统发布：复制到 {tmp:?} 失败: {e}"
        )));
    }

    let outcome = match std::fs::hard_link(&tmp, to) {
        Ok(()) => {
            fsync_dir(Some(dir))?;
            let _ = std::fs::remove_file(from);
            Ok(PublishOutcome::Published)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(PublishOutcome::AlreadyPresent)
        }
        Err(e) => Err(ServerError::Internal(format!(
            "跨文件系统发布：{tmp:?} → {to:?} 失败: {e}"
        ))),
    };
    // 中转文件无论成败都要清掉：它已经完成使命，留着就是垃圾。
    let _ = std::fs::remove_file(&tmp);
    outcome
}

/// 在 `dir` 里**独占创建**一个中转文件，返回打开的句柄和它的路径。
///
/// 🔴 唯一性必须由 `create_new`（`O_EXCL`）保证，不能靠「pid + 时间戳大概率不重」。
/// 同一个进程里两个并发发布可以在同一纳秒取到同一个名字，而 `File::create` 是
/// **截断**语义：后者会把前者复制到一半的内容清空，两边继续往同一个 fd 写，最后
/// 发布出去的是两份字节缝在一起的东西——它的摘要谁都对不上，却已经进了正式路径。
///
/// 计数器只是用来减少重试次数；正确性来自内核的 `O_EXCL`。
fn create_exclusive_temp(dir: &std::path::Path) -> Result<(std::fs::File, std::path::PathBuf)> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let mut last = None;
    for _ in 0..64 {
        let name = format!(
            ".publish-{}-{}-{}.tmp",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            SEQ.fetch_add(1, Ordering::Relaxed),
        );
        let path = dir.join(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(f) => return Ok((f, path)),
            // 撞名了：换一个再来，绝不去打开已经存在的那个。
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => last = Some(e),
        }
    }
    Err(ServerError::Internal(format!(
        "在 {dir:?} 里创建中转文件失败: {last:?}"
    )))
}

/// 逐级创建目录，**每新建一级都同步它的父目录**。
///
/// 🔴 `create_dir_all` 可能一口气建出好几级，而只 fsync 最后那一级的父目录，只保住了
/// 最里面那个目录项。掉电时更外层的目录项照样可能没了，整条路径连同下面的一切一起消失
/// ——而 PG 里的记录已经提交，又回到「记录在、对象不在」。持久化链上少一环等于没有。
fn create_dir_all_synced(path: &std::path::Path) -> std::io::Result<()> {
    create_dir_all_with_sync(path, &mut |dir| std::fs::File::open(dir)?.sync_all())
}

/// [`create_dir_all_synced`] 的可注入版本：`sync` 收到每一个**需要被同步的父目录**。
///
/// 分出来是为了让测试能证明「同步真的发生了」。只断言「目录存在」的话，把同步整段
/// 删掉测试照样绿——那种测试保护不了任何东西。
fn create_dir_all_with_sync(
    path: &std::path::Path,
    sync: &mut impl FnMut(&std::path::Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    // 🔴 **不能因为「目录已经在」就直接返回。**
    //
    // 目录存在只说明有人 `mkdir` 过，**不说明那个目录项落盘了**：先建的那个进程完全
    // 可能在 fsync 之前就崩了。早返回等于把持久化的责任推给一个已经死掉的进程，
    // 于是谁都没做。同步是幂等的，多做一次远比漏掉一次便宜——尤其这段只在启动时跑。
    if let Some(parent) = parent_to_sync(path) {
        create_dir_all_with_sync(&parent, sync)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // 要确认它**真是目录**：同名文件挡在那儿的话，后面所有写入都会以一种
            // 离病因很远的方式失败。
            if !path.is_dir() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{path:?} 已存在且不是目录"),
                ));
            }
        }
        Err(e) => return Err(e),
    }
    if let Some(parent) = parent_to_sync(path) {
        sync(&parent)?;
    }
    Ok(())
}

/// 该同步哪个目录才能保住 `path` 这个**目录项**。
///
/// 🔴 相对路径的最外层（比如 `storage/files` 里的 `storage`）拿到的 parent 是**空串**，
/// 早先当成「没有父目录」跳过了——于是最外面那一级的目录项从来没落过盘，掉电时整棵
/// 目录连同下面的一切一起消失。空 parent 的真实含义是**当前工作目录**，不是「没有」。
fn parent_to_sync(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let parent = match path.parent() {
        Some(p) if p.as_os_str().is_empty() => std::path::PathBuf::from("."),
        // 根目录（`/`）自己没有父目录，也不需要谁来保它。
        Some(p) => p.to_path_buf(),
        None => return None,
    };
    // `.` 的父目录还是 `.`，不打住就是无限递归。
    if parent == path {
        return None;
    }
    Some(parent)
}

/// 把文件内容刷到盘上。
fn fsync_file(path: &std::path::Path) -> Result<()> {
    let f = std::fs::File::open(path)
        .map_err(|e| ServerError::Internal(format!("打开待同步文件 {path:?} 失败: {e}")))?;
    f.sync_all()
        .map_err(|e| ServerError::Internal(format!("同步文件 {path:?} 失败: {e}")))
}

/// 把目录项刷到盘上。
fn fsync_dir(dir: Option<&std::path::Path>) -> Result<()> {
    let Some(dir) = dir else { return Ok(()) };
    let d = std::fs::File::open(dir)
        .map_err(|e| ServerError::Internal(format!("打开目录 {dir:?} 失败: {e}")))?;
    d.sync_all()
        .map_err(|e| ServerError::Internal(format!("同步目录 {dir:?} 失败: {e}")))
}

/// 核验与发布时的分块大小：整个对象绝不一次性进内存。
const VERIFY_CHUNK: u64 = 1 << 20;

/// 崩溃注入点。**只在 debug 构建里存在**，release 是空函数。
///
/// 🔴 上传的崩溃安全性全在几个窄窗口上：校验完还没发布、发布完还没落库、落库完
/// 还没立墓碑。这些窗口用「模拟」是测不出来的——同进程里 `Drop` 一定会跑，异步任务
/// 也会被规规矩矩地取消，而真实事故是进程**当场消失**。所以测试要能在指定窗口把
/// 服务端进程 `abort()` 掉，然后从磁盘和数据库的**实际残留**去验恢复。
#[cfg(debug_assertions)]
pub(crate) fn crash_point(name: &str) {
    if std::env::var("PRIVCHAT_CRASH_POINT").ok().as_deref() == Some(name) {
        eprintln!("💥 崩溃注入点命中：{name}");
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
pub(crate) fn crash_point(_name: &str) {}

/// 正式路径上那个对象是不是**这次要发布的东西**。
///
/// 用于「上次已发布、PG 未提交」的恢复窗口：一致就直接继续落库，不重传也不覆盖。
/// 🔴 只比大小不够——同样长度的不同内容必须判为不一致。
async fn verify_object(
    op: &Operator,
    final_path: &str,
    expect_size: u64,
    expect_sha256: &str,
) -> Result<bool> {
    let meta = match op.stat(final_path).await {
        Ok(m) => m,
        Err(_) => return Ok(false),
    };
    if meta.content_length() != expect_size {
        return Ok(false);
    }
    // 🔴 **分块读**。整个对象一次性进内存的话，一个 200MB 的附件在并发恢复时会把
    // 内存放大成几百 MB——恢复路径恰恰是在故障之后跑的，那正是最不该雪上加霜的时候。
    let reader = op
        .reader(final_path)
        .await
        .map_err(|e| ServerError::Internal(format!("核验已发布对象失败: {e}")))?;
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut offset = 0u64;
    while offset < expect_size {
        let end = (offset + VERIFY_CHUNK).min(expect_size);
        let buf = reader
            .read(offset..end)
            .await
            .map_err(|e| ServerError::Internal(format!("核验已发布对象失败: {e}")))?;
        for chunk in buf {
            sha2::Digest::update(&mut hasher, &chunk);
        }
        offset = end;
    }
    let actual = hex::encode(sha2::Digest::finalize(hasher));
    Ok(actual.eq_ignore_ascii_case(expect_sha256))
}

/// 发布结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublishOutcome {
    /// 这次真的发布了。
    Published,
    /// 正式路径上已经有对象——可能是上次「已发布未提交」留下的，必须核验后再决定。
    AlreadyPresent,
}

impl FileService {
    pub fn new(
        sources: Vec<FileStorageSourceConfig>,
        default_storage_source_id: u32,
        pool: Arc<sqlx::PgPool>,
    ) -> Self {
        let sources_by_id = sources.into_iter().map(|s| (s.id, s)).collect();
        Self {
            sources_by_id,
            operators: Arc::new(RwLock::new(HashMap::new())),
            default_storage_source_id,
            file_upload_repo: Arc::new(FileUploadRepository::new(pool)),
        }
    }

    pub fn source_count(&self) -> usize {
        self.sources_by_id.len()
    }

    fn resolve_storage_source(&self) -> Result<&FileStorageSourceConfig> {
        self.sources_by_id
            .get(&self.default_storage_source_id)
            .ok_or_else(|| {
                ServerError::Internal(format!(
                    "未找到存储源 id={}，请确保 [file] 中至少配置一个 [[file.storage_sources]] 且 default_storage_source_id 存在",
                    self.default_storage_source_id
                ))
            })
    }

    /// 初始化：为每个存储源构建 OpenDAL Operator（Fs 或 S3），本地 FS 预创建子目录
    pub async fn init(&self) -> Result<()> {
        let subdirs = [
            "images/",
            "videos/",
            "audios/",
            "files/",
            "others/",
            "thumbnails/",
        ];
        for src in self.sources_by_id.values() {
            let op = Self::build_operator(src).await?;
            if src.storage_type == "local" {
                for d in &subdirs {
                    op.create_dir(*d).await.map_err(|e| {
                        ServerError::Internal(format!(
                            "创建存储子目录 \"{}\"（存储源 id={}）失败: {}",
                            d.trim_end_matches('/'),
                            src.id,
                            e
                        ))
                    })?;
                }
                // 🔴 **子目录本身的目录项也要落盘。**
                //
                // 发布时 fsync 的是 `images/` 这类目标目录，可它们是在这里创建的，
                // 创建之后从没同步过存储根。掉电时丢的就是这一层目录项——正式路径
                // 整个不存在，而 PG 里的记录已经提交，又变回「记录在、对象不在」。
                fsync_dir(Some(std::path::Path::new(&src.storage_root)))?;
            }
            self.operators.write().await.insert(src.id, op);
        }
        Ok(())
    }

    /// 根据配置构建 OpenDAL Operator（兼容标准 Fs / S3 配置）
    async fn build_operator(src: &FileStorageSourceConfig) -> Result<Operator> {
        if src.storage_type == "local" {
            let root = src.storage_root.trim();
            if root.is_empty() {
                return Err(ServerError::Internal(
                    "local 存储源缺少 storage_root".to_string(),
                ));
            }
            let root_path = std::path::Path::new(root);
            // 目录不存在时自动创建，创建失败则返回明确错误
            if !root_path.exists() {
                create_dir_all_synced(root_path).map_err(|e| {
                    ServerError::Internal(format!("创建文件存储目录失败 \"{}\": {}", root, e))
                })?;
            }
            let abs_root = if root_path.is_absolute() {
                root.to_string()
            } else {
                tokio::fs::canonicalize(root_path)
                    .await
                    .map_err(|e| {
                        ServerError::Internal(format!("无法解析 storage_root \"{}\": {}", root, e))
                    })?
                    .to_string_lossy()
                    .to_string()
            };
            let builder = opendal::services::Fs::default().root(&abs_root);
            let op: Operator = Operator::new(builder)
                .map_err(|e| ServerError::Internal(format!("构建 Fs Operator 失败: {}", e)))?
                .finish();
            return Ok(op);
        }
        if src.storage_type == "s3" {
            let endpoint = src
                .endpoint
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 endpoint".to_string()))?
                .trim();
            let bucket = src
                .bucket
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 bucket".to_string()))?
                .trim();
            let access_key_id = src
                .access_key_id
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 access_key_id".to_string()))?
                .trim();
            let secret_access_key = src
                .secret_access_key
                .as_deref()
                .ok_or_else(|| {
                    ServerError::Internal("S3 存储源缺少 secret_access_key".to_string())
                })?
                .trim();
            if endpoint.is_empty()
                || bucket.is_empty()
                || access_key_id.is_empty()
                || secret_access_key.is_empty()
            {
                return Err(ServerError::Internal(
                    "S3 存储源 endpoint / bucket / access_key_id / secret_access_key 均不能为空"
                        .to_string(),
                ));
            }
            let endpoint_url =
                if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                    endpoint.to_string()
                } else {
                    format!("https://{}", endpoint)
                };
            let mut builder = opendal::services::S3::default()
                .bucket(bucket)
                .endpoint(&endpoint_url)
                .region("auto")
                .access_key_id(access_key_id)
                .secret_access_key(secret_access_key);
            if let Some(ref prefix) = src.path_prefix {
                let p = prefix.trim().trim_end_matches('/');
                if !p.is_empty() {
                    builder = builder.root(p);
                }
            }
            let op: Operator = Operator::new(builder)
                .map_err(|e| ServerError::Internal(format!("构建 S3 Operator 失败: {}", e)))?
                .finish();
            return Ok(op);
        }
        Err(ServerError::Unsupported(format!(
            "不支持的存储类型: {}",
            src.storage_type
        )))
    }

    /// P0-10 流式上传起点：确定类型/存储源/file_id/路径，打开流式 writer。
    /// 数据不再一次性进内存——调用方循环 `write_chunk` 边收边写，超限即时中止。
    /// `token_max_size` 与按类型的服务端上限取 min 作为硬顶。
    pub async fn begin_streaming_upload(
        &self,
        mime_type: &str,
        filename: &str,
        token_max_size: i64,
        // 崩溃重试时复用的 file_id（会话 `state.json` 里预留的那个）；
        // `None` = 首次，现分配。
        reserved_file_id: Option<u64>,
        // token 里签下的语义类型。🔴 优先于 multipart MIME：
        // 加密上传的 body 是不透明字节，客户端普遍标成 application/octet-stream，
        // 按它推导会把加密后的图片存成普通文件，之后按 image 预检自然对不上。
        // multipart 只负责承载字节，「这是什么」由签名 token 说了算。
        token_file_type: Option<FileType>,
        // 会话身份：临时对象落在这个会话目录下。
        session_uid: u64,
        session_upload_id: &str,
    ) -> Result<StreamingUpload> {
        let file_type = match token_file_type {
            Some(ft) => ft,
            None => self.detect_file_type(mime_type)?,
        };
        let type_limit = Self::max_size_for_type(&file_type) as u64;
        let limit = type_limit.min(token_max_size.max(0) as u64);

        let source = self.resolve_storage_source()?;
        let source_id = source.id;
        let op = self
            .operators
            .read()
            .await
            .get(&source_id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={} 的 Operator", source_id))
            })?;

        // 🔴 崩溃重试要复用**同一个** file_id：会话把它记在 `state.json` 里，
        // 于是「上次其实已经落库了」在提交时表现为主键冲突而不是第二条记录。
        let file_id = match reserved_file_id {
            Some(id) => id,
            None => self.file_upload_repo.next_file_id().await?,
        };
        let file_path = self.generate_file_path(file_id, &file_type, filename);

        // 🔴 **字节先写会话临时对象，校验通过后才发布到正式路径。**
        //
        // 早先直接对着正式路径开 writer：崩溃重试会覆盖一个可能已被提交记录引用的
        // 对象，失败时 `abort()` 还会把它删掉。会话临时路径与正式对象在**同一个
        // operator 根**下，所以发布是一次同盘 rename，不多一次拷贝。
        let staging_path = Self::staging_path(session_uid, session_upload_id);
        let writer = op
            .writer(&staging_path)
            .await
            .map_err(|e| ServerError::Internal(format!("打开存储 writer 失败: {}", e)))?;

        Ok(StreamingUpload {
            file_id,
            file_path,
            staging_path,
            source_id,
            file_type,
            op,
            writer: Some(writer),
            hasher: <sha2::Sha256 as sha2::Digest>::new(),
            written: 0,
            limit,
        })
    }

    /// P0-10 流式上传收尾：关闭 writer、定稿 hash、落库返回元数据。
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_streaming_upload(
        &self,
        mut upload: StreamingUpload,
        filename: String,
        mime_type: String,
        uploader_id: u64,
        uploader_ip: Option<String>,
        business_type: String,
        business_id: Option<String>,
        encryption_version: i32,
        cek: Option<String>,
        // 产出这份字节的客户端处理版本；0 = 原始未处理。
        transform_version: i32,
        // 客户端在 prepare 声明并签进 token 的**最终上传 blob** 摘要。
        // `None` = 老客户端没报，这次上传不参与秒传。
        declared_content_sha256: Option<String>,
        // token 里签下的精确字节数；`None` = 老客户端没报。
        declared_size: Option<i64>,
    ) -> Result<FileMetadata> {
        let mut writer = upload
            .writer
            .take()
            .ok_or_else(|| ServerError::Internal("上传流已关闭".to_string()))?;
        writer
            .close()
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入收尾失败: {}", e)))?;

        // 🔴 权威摘要由**服务端**计算，对象是**实际收到并落盘的那串字节**。
        //
        // 服务端不理解加密，也不需要理解：去重的单位就是「最终上传的字节」。
        // 字节完全相同才复用，因此——
        //   · 明文文件与加密文件不会互相命中；
        //   · 同一明文用不同随机 CEK/nonce 加密两次，是**两个**物理文件，这是预期行为。
        //
        // 客户端要拿到秒传，就必须**保留并重传当初参与哈希的那个 blob**：
        // 预检之后重新加密一次，字节就变了，本来也不该命中。
        //
        // 客户端在 prepare 报的值只用于预检，不能直接写库——那等于让调用方自己
        // 声明「我这份字节叫什么」，之后别人算出同一个名字就会拿到他的东西。
        let stored_sha256 = hex::encode(sha2::Digest::finalize(upload.hasher));

        if let Some(declared_size) =
            size_check_target(declared_content_sha256.as_deref(), declared_size)
        {
            if declared_size != upload.written as i64 {
                // 删的是**临时对象**：这一刻正式路径上什么都没有，字节还没发布。
                if let Ok(op) = self.operator_for_source(upload.source_id).await {
                    let _ = op.delete(&upload.staging_path).await;
                }
                return Err(ServerError::Validation(format!(
                    "上传字节数与 prepare 阶段声明的不一致：声明 {declared_size}，实际 {}",
                    upload.written
                )));
            }
        }

        if let Some(declared) = declared_content_sha256.as_deref() {
            if !declared.eq_ignore_ascii_case(&stored_sha256) {
                // 声明与实际不符：删临时对象。正式路径从头到尾没被碰过。
                if let Ok(op) = self.operator_for_source(upload.source_id).await {
                    let _ = op.delete(&upload.staging_path).await;
                }
                return Err(ServerError::Validation(
                    "上传内容与 prepare 阶段声明的摘要不一致".to_string(),
                ));
            }
        }

        // 窗口一：字节收完并校验通过，但还没发布。
        crash_point("after_verify_before_publish");

        // 并发首传收敛（见 `converge_upload`）。
        let mut tx = self
            .file_upload_repo
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启上传收敛事务失败: {e}")))?;
        let placement = converge_upload(
            &mut tx,
            &UploadPlacement {
                stored_sha256: stored_sha256.clone(),
                encryption_version,
                my_path: upload.file_path.clone(),
                my_source_id: upload.source_id as i32,
                my_cek: cek,
            },
        )
        .await?;
        let (file_path, source_id, enc_version, stored_cek, duplicate) = (
            placement.file_path.clone(),
            placement.storage_source_id,
            placement.encryption_version,
            placement.cek.clone(),
            placement.duplicate,
        );

        // ---- 校验通过 → 发布 ----
        //
        // 📌 **这里没有持久化的状态机**，`UploadStatus` 只有 `WholeReceiving` 和
        // `Completed`。整包路径不需要更细的状态：请求是一次性的 HTTP POST，崩溃之后
        // 客户端**必然**要重发整个 body（它没有别的办法把字节再交给服务端一次）。
        // 「已发布未提交」的恢复因此省掉的是**重复发布与重复插入**，不是重传——
        // 断点续传是分片路径的事，别把那条承诺挂到这里。
        //
        // 🔴 **发布排在落库之前**：允许「对象在、记录不在」（孤儿由清理任务回收），
        // 绝不允许「记录在、对象不在」（`file_id` 永久指向空）。
        //
        // 去重命中时**根本不发布**：字节已经有人存着了，这次的临时对象直接删掉。
        if duplicate {
            if let Ok(op) = self.operator_for_source(upload.source_id).await {
                let _ = op.delete(&upload.staging_path).await;
            }
            tracing::info!("⚡ 内容已存在，复用 path={file_path}，不发布新对象");
        } else {
            match self
                .publish_staged(upload.source_id, &upload.staging_path, &upload.file_path)
                .await?
            {
                PublishOutcome::Published => {}
                // 🔴 **恢复窗口**：上一次「已发布、PG 未提交」就崩了。同盘发布之后
                // 临时对象已经不在，正式路径上却有东西。
                //
                // 流式核验大小与摘要：一致就直接继续落库（不重复发布、不覆盖）；
                // 不一致说明这个路径上是**别的内容**，报冲突——覆盖它就是拿新字节
                // 顶掉可能正被引用的文件。
                PublishOutcome::AlreadyPresent => {
                    let same = self
                        .verify_published(
                            upload.source_id,
                            &upload.file_path,
                            upload.written,
                            &stored_sha256,
                        )
                        .await?;
                    if !same {
                        return Err(ServerError::Internal(format!(
                            "正式路径 {} 上已有**不同内容**的对象，拒绝覆盖",
                            upload.file_path
                        )));
                    }
                    // 一致：上次发布过了。把这次的临时对象清掉即可。
                    if let Ok(op) = self.operator_for_source(upload.source_id).await {
                        let _ = op.delete(&upload.staging_path).await;
                    }
                    tracing::info!(
                        "♻️ 正式路径已存在且内容一致（上次已发布未提交），直接继续落库"
                    );
                }
            }
        }

        // 窗口二：对象已经在正式路径上了，事务还没提交。
        crash_point("after_publish_before_commit");

        let metadata = FileMetadata {
            file_id: upload.file_id,
            original_filename: filename,
            file_size: upload.written,
            original_size: None,
            file_type: upload.file_type.clone(),
            mime_type,
            file_path: file_path.clone(),
            storage_source_id: source_id as u32,
            uploader_id,
            uploader_ip,
            uploaded_at: chrono::Utc::now().timestamp_millis() as u64,
            width: None,
            height: None,
            file_hash: Some(stored_sha256.clone()),
            business_type: Some(business_type),
            business_id,
            encryption_version: enc_version,
            cek: stored_cek,
        };
        // 🔴 幂等完全靠**已有的主键**。`file_id` 在收 body 之前就分配好并记进会话
        // （`state.json` 的 `reserved_file_id`），重试复用同一个 id；于是「上一次其实
        // 已经落库了」在这里表现为主键冲突 → 0 行 → 回读那一行返回。
        //
        // 📌 **不为此在正式文件表上加任何列**：上传中间态是可丢弃的临时数据，
        // 不进业务库。（本批曾加过 `upload_completion_key` 列 + 索引，属过度设计，已撤销。）
        let inserted = self.insert_within(&mut tx, &metadata).await?;
        let metadata = if inserted {
            metadata
        } else {
            // 🔴 主键冲突**只有在既有记录与本次身份一致时**才算「上次已落库」的幂等成功。
            // 不核对就直接返回，等于把另一条文件记录交给当前调用者。
            let existing = self
                .file_upload_repo
                .get_by_file_id(metadata.file_id)
                .await?
                .ok_or_else(|| {
                    ServerError::Internal(format!("主键冲突却读不到 {}", metadata.file_id))
                })?;
            let same_identity = existing.uploader_id == metadata.uploader_id
                && existing.file_hash == metadata.file_hash
                && existing.file_size == metadata.file_size
                && existing.file_type.as_str() == metadata.file_type.as_str();
            if !same_identity {
                return Err(ServerError::Internal(format!(
                    "file_id={} 已被另一份内容占用（uploader/摘要/大小/类型不符）",
                    metadata.file_id
                )));
            }
            tracing::info!("♻️ file_id={} 上次已落库且身份一致，回读既有记录", metadata.file_id);
            existing
        };
        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交上传收敛事务失败: {e}")))?;

        Ok(metadata)
    }

    /// 按存储源取 Operator。
    async fn operator_for_source(&self, source_id: u32) -> Result<Operator> {
        self.operators
            .read()
            .await
            .get(&source_id)
            .cloned()
            .ok_or_else(|| ServerError::Internal(format!("未找到存储源 id={source_id} 的 Operator")))
    }

    /// 在给定事务里插入上传记录。收敛判定与插入必须同事务，否则锁白加。
    async fn insert_within(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        meta: &FileMetadata,
    ) -> Result<bool> {
        let done = sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_size, file_type, mime_type,
                file_path, storage_source_id, uploader_id, uploader_ip, uploaded_at,
                width, height, file_hash, business_type, business_id,
                encryption_version, cek
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
            ON CONFLICT (file_id) DO NOTHING
            "#,
        )
        .bind(meta.file_id as i64)
        .bind(&meta.original_filename)
        .bind(meta.file_size as i64)
        .bind(meta.file_type.as_str())
        .bind(&meta.mime_type)
        .bind(&meta.file_path)
        .bind(meta.storage_source_id as i32)
        .bind(meta.uploader_id as i64)
        .bind(&meta.uploader_ip)
        .bind(meta.uploaded_at as i64)
        .bind(meta.width.map(|v| v as i32))
        .bind(meta.height.map(|v| v as i32))
        .bind(&meta.file_hash)
        .bind(&meta.business_type)
        .bind(&meta.business_id)
        .bind(meta.encryption_version)
        .bind(&meta.cek)
        .execute(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("插入上传记录失败: {e}")))?;
        // 0 行 = 这个 file_id 上次已落库（崩溃重试复用了预留 id），回读而不是当新插入。
        Ok(done.rows_affected() > 0)
    }

    /// 秒传探测：这份内容在不在（不写任何东西）。
    pub async fn find_by_content(
        &self,
        sha256: &str,
    ) -> Result<Option<crate::model::file_upload::FileMetadata>> {
        self.file_upload_repo.find_by_content(sha256).await
    }

    /// 同一份内容的所有逻辑记录（有上限）。授权按记录算，调用者能读的未必是最老那条。
    pub async fn find_all_by_content(
        &self,
        sha256: &str,
    ) -> Result<Vec<crate::model::file_upload::FileMetadata>> {
        self.file_upload_repo.find_all_by_content(sha256).await
    }

    /// 秒传取用：照着已有那行给当前用户插一条新记录，物理文件不动。
    pub async fn copy_for_user(
        &self,
        source: &crate::model::file_upload::FileMetadata,
        uploader_id: u64,
        business_type: &str,
        claim_key_hash: Option<&str>,
    ) -> Result<u64> {
        self.file_upload_repo
            .copy_for_user(source, uploader_id, business_type, claim_key_hash)
            .await
    }

    /// 这个幂等键之前是不是已经成功取用过。
    pub async fn find_claimed(&self, uploader_id: u64, claim_key_hash: &str) -> Result<Option<u64>> {
        self.file_upload_repo
            .find_claimed(uploader_id, claim_key_hash)
            .await
    }

    /// 预分配一个 `file_id`（在接收字节之前）。
    pub async fn reserve_file_id(&self) -> Result<u64> {
        self.file_upload_repo.next_file_id().await
    }

        /// 上传会话临时目录的根（`tmp/uploads/`）。
    ///
    /// 挂在**默认本地存储源**的 root 之下：与最终对象同一个文件系统时，
    /// complete 的发布就是一次 rename（RESUMABLE_UPLOAD_SPEC §9.2）。
    /// 跨文件系统时走复制降级，那条路径在分片批次里落地。
    pub fn upload_session_root(&self) -> Result<std::path::PathBuf> {
        let src = self
            .sources_by_id
            .get(&self.default_storage_source_id)
            .ok_or_else(|| ServerError::Internal("找不到默认存储源".to_string()))?;
        if src.storage_type != "local" {
            // 对象存储后端的临时目录仍落本机磁盘（会话是节点本地的）。
            return Ok(std::path::PathBuf::from("./storage/tmp/uploads"));
        }
        Ok(std::path::Path::new(&src.storage_root).join("tmp/uploads"))
    }

    pub async fn get_file_metadata(&self, file_id: u64) -> Result<Option<FileMetadata>> {
        self.file_upload_repo.get_by_file_id(file_id).await
    }

    pub async fn update_business(
        &self,
        file_id: u64,
        business_type: &str,
        business_id: &str,
    ) -> Result<bool> {
        self.file_upload_repo
            .update_business(file_id, business_type, business_id)
            .await
    }

    pub async fn list_file_ids_by_business(
        &self,
        business_type: &str,
        business_id: &str,
    ) -> Result<Vec<u64>> {
        self.file_upload_repo
            .list_file_ids_by_business(business_type, business_id)
            .await
    }

    pub async fn verify_file_ownership(&self, file_id: u64, user_id: u64) -> Result<bool> {
        Ok(self
            .file_upload_repo
            .get_by_file_id(file_id)
            .await?
            .map(|m| m.uploader_id == user_id)
            .unwrap_or(false))
    }

    /// 直接物理删除文件——**已停用**（MEDIA_REFERENCE_AND_FORWARD_SPEC §8.2）。
    ///
    /// 共享引用模型下「我上传的文件我能删」不成立：一个文件可能同时被原消息和
    /// 若干转发副本引用，删掉物理文件会让那些副本一起变成打不开的图。
    ///
    /// 「先数引用再删」也不够——两步之间可以插入一条新引用（转发只要一个事务），
    /// 删除照样发生。要做对必须是 GC 状态机：`status=gc_pending` + 宽限期 +
    /// 到点复查引用，全程可被新引用取消。
    ///
    /// 在那套状态机落地之前这里直接拒绝。**不做 ownership / 引用计数查询**——
    /// 查了也不影响结果，只是让人误以为这里还有一套判断在生效。
    /// 现状：本方法无 RPC 调用方，这是拆引信，不是砍功能。
    pub async fn delete_file(&self, file_id: u64, user_id: u64) -> Result<()> {
        let Some(meta) = self.file_upload_repo.get_by_file_id(file_id).await? else {
            return Ok(());
        };
        if meta.uploader_id != user_id {
            return Err(ServerError::Forbidden("只能删除自己的文件".to_string()));
        }

        // 🔴 先看还有没有别人指着同一个物理文件，再删自己这行——顺序反过来的话，
        // 两个人同时删各自的行会双双读到「还有别人」，物理文件永远没人删。
        // 反过来说，先删行再判断也不行：两边都判成「没人了」，然后都去删同一个对象。
        //
        // 这里用一次 SELECT ... FOR UPDATE 把同 path 的行锁住，让删除与秒传取用
        // （它会插入新的一行指向同一个 path）排成序。
        let mut tx = self
            .file_upload_repo
            .pool()
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启删除事务失败: {e}")))?;

        // 🔴 `COUNT(*) ... FOR UPDATE` **不构成任何锁**：聚合结果没有行可锁，
        // Postgres 直接把它当普通查询。上一版写成那样，等于两个并发删除都读到
        // 「还有别人」（各自看见对方的行），物理文件永远没人删；或者都读到
        // 「没人了」，双双去删同一个对象。
        //
        // 用按 file_path 的 advisory 锁把同一个物理文件上的删除与秒传取用串起来。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&meta.file_path)
            .execute(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("获取物理文件锁失败: {e}")))?;

        let others: (i64,) = sqlx::query_as(
            "SELECT count(*) FROM privchat_file_uploads \
             WHERE file_path = $1 AND file_id <> $2",
        )
        .bind(&meta.file_path)
        .bind(file_id as i64)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("统计共享物理文件的记录失败: {e}")))?;

        sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
            .bind(file_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("删除上传记录失败: {e}")))?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交删除事务失败: {e}")))?;

        if others.0 > 0 {
            tracing::info!(
                "🗑️ 只删记录 file_id={file_id}：还有 {} 行指向同一个物理文件",
                others.0
            );
            return Ok(());
        }

        // 最后一行没了才删物理文件。删失败只记日志：数据库已经提交，
        // 再回滚出一行"指向不存在文件"的记录更糟；留下的孤儿对象由 GC 收。
        let op = self.operator_for_source(meta.storage_source_id).await?;
        if let Err(e) = op.delete(&meta.file_path).await {
            tracing::warn!("删除物理文件失败（留待 GC）path={}: {}", meta.file_path, e);
        }
        Ok(())
    }

    fn detect_file_type(&self, mime_type: &str) -> Result<FileType> {
        // 注：这里只按 MIME 服务端兜底分类。Voice 消息的分类由 SDK 明确传入 "voice"，
        // 不靠 MIME 推导——否则任何 audio/* 的普通文件会被误分到 Voice。
        if mime_type.starts_with("image/") {
            Ok(FileType::Image)
        } else if mime_type.starts_with("video/") {
            Ok(FileType::Video)
        } else {
            Ok(FileType::File)
        }
    }

    /// 按文件类型的服务端大小硬顶（流式路径在 write_chunk 中即时校验）。
    ///
    /// 与签发 token 时用的是同一个函数（[`FileType::max_size_bytes`]）——两处一旦分家，
    /// 松的那个会放进来一批注定失败的上传。
    fn max_size_for_type(file_type: &FileType) -> usize {
        file_type.max_size_bytes() as usize
    }

    /// 会话临时对象在 operator 里的相对路径。
    ///
    /// 与 [`Self::upload_session_root`] 指的是同一个目录，只是这里用 operator 相对路径
    /// 表达——发布因此是同一文件系统内的 rename，不需要复制。
    fn staging_path(uid: u64, upload_id: &str) -> String {
        format!("tmp/uploads/{uid}/{upload_id}/body.part")
    }

    /// 把校验通过的临时对象发布到正式路径（薄委托，语义见 [`publish_object`]）。
    async fn publish_staged(
        &self,
        source_id: u32,
        staging: &str,
        final_path: &str,
    ) -> Result<PublishOutcome> {
        let op = self.operator_for_source(source_id).await?;
        publish_object(&op, self.local_root_of(source_id).as_deref(), staging, final_path).await
    }

    /// 本地存储源的根目录；非 local 返回 `None`。
    fn local_root_of(&self, source_id: u32) -> Option<String> {
        self.sources_by_id
            .get(&source_id)
            .filter(|s| s.storage_type == "local")
            .map(|s| s.storage_root.clone())
    }

    /// 核验正式路径上的对象是不是这次要发布的东西（薄委托，见 [`verify_object`]）。
    async fn verify_published(
        &self,
        source_id: u32,
        final_path: &str,
        expect_size: u64,
        expect_sha256: &str,
    ) -> Result<bool> {
        let op = self.operator_for_source(source_id).await?;
        verify_object(&op, final_path, expect_size, expect_sha256).await
    }

    fn generate_file_path(&self, file_id: u64, file_type: &FileType, filename: &str) -> String {
        let extension = filename.split('.').last().unwrap_or("bin");
        let subdir = match file_type {
            FileType::Image => "images",
            FileType::Video => "videos",
            FileType::Voice => "voices",
            FileType::File => "files",
            FileType::Other => "others",
        };
        format!("{}/{}.{}", subdir, file_id, extension)
    }

    pub async fn get_file_url(&self, file_id: u64, _user_id: u64) -> Result<FileUrlResponse> {
        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;
        let file_url = self.build_access_url(&metadata.file_path, metadata.storage_source_id);
        let expires_at = Utc::now().timestamp() + 3600 * 24 * 365;
        Ok(FileUrlResponse {
            file_url,
            thumbnail_url: None,
            expires_at,
            file_size: metadata.file_size,
            mime_type: metadata.mime_type,
            storage_source_id: metadata.storage_source_id,
            // ⚠️ P0 安全：此处随 detail 返回 CEK，但调用方 get_file_url 目前 **未做** 访问授权
            // （user_id 未使用）。返回 cek 前必须校验当前用户有权访问该附件（file→message→channel
            // 成员）。授权补齐前，加密形同虚设。见 ATTACHMENT_ENCRYPTION_SPEC §授权。
            encryption_version: metadata.encryption_version,
            cek: metadata.cek,
        })
    }

    /// 读取文件内容（用于下载；统一走 OpenDAL read）
    pub async fn read_file(&self, file_id: u64) -> Result<Vec<u8>> {
        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;
        let op = self
            .operators
            .read()
            .await
            .get(&metadata.storage_source_id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={}", metadata.storage_source_id))
            })?;

        let buf = op
            .read(&metadata.file_path)
            .await
            .map_err(|e| ServerError::Internal(format!("存储读取失败: {}", e)))?;
        Ok(buf.to_vec())
    }

    pub fn build_access_url(&self, file_path: &str, storage_source_id: u32) -> String {
        if let Some(src) = self.sources_by_id.get(&storage_source_id) {
            if let Some(base_url) = &src.base_url {
                let base = base_url.trim_end_matches('/');
                // base_url 已包含完整路径，直接拼接 file_path
                return format!("{}/{}", base, file_path);
            }
            return format!("/{}", file_path);
        }
        format!("{{unsupported:storage_source_id={}}}", storage_source_id)
    }
}

/// P0-10 流式上传句柄：由 `begin_streaming_upload` 创建，调用方按 chunk 喂数据，
/// 全程只驻留单个 chunk 的内存。成功走 `commit_streaming_upload` 落库；
/// 失败/校验不过必须调 `abort()` 清掉已写入的半文件。
pub struct StreamingUpload {
    pub file_id: u64,
    /// 校验通过后要发布到的**正式**路径（由 `file_id` 决定，重试恒定）。
    pub file_path: String,
    /// 字节先落在这里（会话临时对象）。与正式路径同一个 operator 根。
    pub staging_path: String,
    pub source_id: u32,
    file_type: FileType,
    op: Operator,
    writer: Option<opendal::Writer>,
    /// 🔴 内容摘要必须是 **SHA-256**，不能用 `DefaultHasher`。
    ///
    /// `DefaultHasher` 是 SipHash：标准库明确写着**不保证跨 Rust 版本稳定**，
    /// 只有 64 位，也不是密码学摘要。拿它当文件内容标识，秒传会在某次工具链升级后
    /// 集体失配（同一个文件算出不同值 → 全量重传），碰撞也不是理论问题。
    hasher: sha2::Sha256,
    written: u64,
    limit: u64,
}

impl StreamingUpload {
    /// 已写入字节数（加密结构等收尾校验用）。
    pub fn written(&self) -> u64 {
        self.written
    }

    /// 写入一个 chunk：先做累计大小硬顶校验（超限即时失败，不再继续收 body），
    /// 同步推进增量 hash。
    pub async fn write_chunk(&mut self, chunk: bytes::Bytes) -> Result<()> {
        self.written = self.written.saturating_add(chunk.len() as u64);
        if self.written > self.limit {
            return Err(ServerError::Validation(format!(
                "文件大小超过限制: {} > {} bytes",
                self.written, self.limit
            )));
        }
        sha2::Digest::update(&mut self.hasher, &chunk);
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| ServerError::Internal("上传流已关闭".to_string()))?;
        writer
            .write(chunk)
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入失败: {}", e)))
    }

    /// 中止上传：尽力关闭 writer 并删除已写入的半文件（不落库）。
    pub async fn abort(mut self) {
        if let Some(mut writer) = self.writer.take() {
            let _ = writer.close().await;
        }
        // 🔴 删的是**临时对象**。中止时字节还没发布，正式路径上要么什么都没有、
        // 要么是上一次成功发布的那份——后者绝不能碰。早先这里删 `file_path`，
        // 于是一次失败的重试会把一个已被提交记录引用的对象删掉。
        if let Err(e) = self.op.delete(&self.staging_path).await {
            tracing::warn!(
                "⚠️ 清理中止上传的临时对象失败 path={}: {}",
                self.staging_path,
                e
            );
        }
    }
}

#[cfg(test)]
mod authz_tests {
    use super::{authorize_file_access, FileAccessFacts};

    fn facts(
        requester_id: u64,
        uploader_id: u64,
        has_any_reference: bool,
        member_of_live: bool,
    ) -> FileAccessFacts {
        FileAccessFacts {
            requester_id,
            uploader_id,
            has_any_reference,
            requester_is_member_of_a_live_reference: member_of_live,
        }
    }

    // pending（还没被任何消息引用）：仅 uploader 可访问
    #[test]
    fn pending_uploader_allowed() {
        assert!(authorize_file_access(facts(1, 1, false, false)));
    }

    #[test]
    fn pending_non_uploader_denied() {
        assert!(!authorize_file_access(facts(2, 1, false, false)));
    }

    // 被有效消息引用：会话成员可访问
    #[test]
    fn referenced_member_allowed() {
        assert!(authorize_file_access(facts(2, 1, true, true)));
    }

    #[test]
    fn referenced_non_member_denied() {
        assert!(!authorize_file_access(facts(2, 1, true, false)));
    }

    // uploader 身份不能绕过成员校验
    #[test]
    fn referenced_uploader_but_non_member_denied() {
        assert!(!authorize_file_access(facts(1, 1, true, false)));
    }

    #[test]
    fn referenced_uploader_and_member_allowed() {
        assert!(authorize_file_access(facts(1, 1, true, true)));
    }

    /// 【spec §4.2 的回归】引用全部失效（撤回/删除）→ 拒绝。
    ///
    /// 这条正是「撤回后附件仍可下载」那个洞：撤回是软删，行还在，
    /// 旧实现裸查 channel_id 拿到会话、成员校验通过，于是照常放行。
    #[test]
    fn every_reference_revoked_denies_even_the_uploader() {
        assert!(!authorize_file_access(facts(1, 1, true, false)));
        assert!(!authorize_file_access(facts(2, 1, true, false)));
    }

    /// 【转发的核心用例】上传者与请求者毫无关系，只要请求者在某条有效引用
    /// 消息的会话里就该放行——转发副本的接收方正是这个形态。
    #[test]
    fn a_shared_file_reference_is_readable_by_another_message_owner() {
        assert!(authorize_file_access(facts(777, 1, true, true)));
    }

    /// 上传摘要必须是 **SHA-256 的十六进制**，秒传要靠它判「同一份内容」。
    ///
    /// 🔴 这里曾经用 `DefaultHasher`，写出来的是 `hash:<u64>`。那是 SipHash：
    /// 标准库明确说**不保证跨 Rust 版本稳定**，只有 64 位，也不是密码学摘要。
    /// 换个工具链重编，同一个文件算出来的值就变了——秒传会从「命中」变成全量重传，
    /// 而且这种失效不会报错，只会悄悄变慢。
    #[test]
    fn the_upload_digest_is_a_sha256_hex_string() {
        use sha2::Digest as _;

        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        hasher.update(b"privchat");
        let digest = hex::encode(hasher.finalize());

        assert_eq!(digest.len(), 64, "SHA-256 十六进制是 64 个字符");
        assert!(
            digest.chars().all(|c| c.is_ascii_hexdigit()),
            "必须是纯十六进制，不能带 `hash:` 之类前缀——那种值没法跨端比对",
        );
        // 已知向量：换实现或换编码方式都会在这里断掉。
        assert_eq!(
            digest,
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        );
    }
}

/// 一次上传要落到哪个物理文件上的输入。
#[derive(Debug, Clone)]
pub struct UploadPlacement {
    /// 服务端对**实际收到并落盘的字节**算出的 SHA-256。
    pub stored_sha256: String,
    pub encryption_version: i32,
    pub my_path: String,
    pub my_source_id: i32,
    pub my_cek: Option<String>,
}

/// 收敛结果：这条记录最终指向哪个物理文件。
#[derive(Debug, Clone)]
pub struct ResolvedPlacement {
    pub file_path: String,
    pub storage_source_id: i32,
    pub encryption_version: i32,
    pub cek: Option<String>,
    /// true = 命中了别人先落的那份，自己刚写的对象可以删。
    pub duplicate: bool,
}

/// 并发首传收敛：同一串字节只保留一份物理文件。
///
/// 两个人同时上传同样的字节，两边预检都没命中，于是各写了一份对象。这里在
/// **内容锁**里再查一次：已经有人先落了就指向他那份，自己那份可以删。
/// 少了这一步，「物理文件只存一份」恰好在并发时不成立——而并发正是它最该成立的时候。
///
/// 🔴 去重的单位是**最终上传的字节**，服务端不理解加密：明文与密文不会互相命中，
/// 同一明文用不同随机 CEK/nonce 加密两次也是两个物理文件——这是预期行为。
/// 客户端要拿到秒传，就得保留并重传当初参与哈希的那个 blob。
///
/// 生产与测试**调用的是同一个函数**。此前测试把这段 SQL 抄了一份，
/// 那样只能证明抄件自洽：改了生产的判据，先红的是测试，而测试证明不了产品。
pub async fn converge_upload(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    input: &UploadPlacement,
) -> Result<ResolvedPlacement> {
    // 同一串字节的所有首传串行到这一把锁上。粒度是内容，不是全表。

    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(&input.stored_sha256)
        .execute(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("获取内容锁失败: {e}")))?;

    // 判重只看 hash：摘要相同即字节相同，大小自然相同，也不存在
    // 「明文和密文互相复用」——字节都一样了，就是同一份东西。
    let existing: Option<(String, i32, i32, Option<String>)> = sqlx::query_as(
        "SELECT file_path, storage_source_id, encryption_version, cek \
         FROM privchat_file_uploads \
         WHERE file_hash = $1 ORDER BY file_id LIMIT 1",
    )
    .bind(&input.stored_sha256)
    .fetch_optional(&mut **tx)
    .await
    .map_err(|e| ServerError::Database(format!("查询同内容文件失败: {e}")))?;

    if let Some((path, src, enc, existing_cek)) = existing {
        // 🔴 选中了别人那份物理文件之后，还要取**同一把 `file_path` 锁**再确认它没被删。
        //
        // 只有内容锁挡不住这条：上传选中旧路径 → 删除把最后一行连同物理对象删掉 →
        // 上传插入一条指向已删除对象的记录。增加引用与减少引用必须共用一把锁。
        //
        // 锁序固定为「内容锁 → 路径锁」，而删除/取用只拿路径锁，因此不会成环。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&path)
            .execute(&mut **tx)
            .await
            .map_err(|e| ServerError::Database(format!("获取物理文件锁失败: {e}")))?;

        let still_there: Option<(i64,)> = sqlx::query_as(
            "SELECT file_id FROM privchat_file_uploads WHERE file_path = $1 LIMIT 1",
        )
        .bind(&path)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|e| ServerError::Database(format!("复查物理文件失败: {e}")))?;

        if still_there.is_some() {
            return Ok(ResolvedPlacement {
                duplicate: path != input.my_path,
                file_path: path,
                storage_source_id: src,
                encryption_version: enc,
                cek: existing_cek,
            });
        }
        // 等锁期间它被删了：退回用自己刚上传的那份，物理文件不丢。
    }

    Ok(ResolvedPlacement {
        file_path: input.my_path.clone(),
        storage_source_id: input.my_source_id,
        encryption_version: input.encryption_version,
        cek: input.my_cek.clone(),
        duplicate: false,
    })
}

/// 这次上传要不要核对声明的字节数，要的话核对哪个值。
///
/// 🔴 只有**同时**带了摘要和大小才核对。老客户端报的是**明文**大小，之后才加密，
/// 密文固定多 28 字节（12 nonce + 16 tag）——无条件比对会让新服务端一上线就把
/// 所有老客户端的正常上传全部拒掉。那不是「秒传暂时不可用」，是附件直接发不出去。
///
/// 抽成函数是为了让测试调**这一个**，而不是在测试里抄一份同样的条件：
/// 抄件只能证明抄件自洽，生产改回无条件核对时它照样绿。
pub fn size_check_target(declared_digest: Option<&str>, declared_size: Option<i64>) -> Option<i64> {
    match (declared_digest, declared_size) {
        (Some(_), Some(size)) => Some(size),
        _ => None,
    }
}

#[cfg(test)]
mod publish_tests {
    use super::{publish_object, verify_object, PublishOutcome};
    use opendal::Operator;

    fn op_at(root: &std::path::Path) -> Operator {
        let builder = opendal::services::Fs::default().root(&root.to_string_lossy());
        Operator::new(builder).expect("fs operator").finish()
    }

    async fn write(op: &Operator, path: &str, bytes: &[u8]) {
        op.write(path, bytes.to_vec()).await.expect("write");
    }

    fn digest(bytes: &[u8]) -> String {
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes))
    }

    /// 发布 = 把临时对象搬到正式路径，**并立即移除临时对象**。
    /// 清理不能只靠 callback：客户端可能上传成功后就离线了。
    #[tokio::test]
    async fn publishing_moves_the_staged_object_and_clears_it() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let root = dir.path().to_string_lossy().to_string();
        write(&op, "tmp/uploads/7/aa/body.part", b"hello").await;

        let out = publish_object(&op, Some(&root), "tmp/uploads/7/aa/body.part", "images/1.png")
            .await
            .expect("publish");
        assert_eq!(out, PublishOutcome::Published);
        assert!(op.stat("images/1.png").await.is_ok(), "正式路径必须有对象");
        assert!(
            op.stat("tmp/uploads/7/aa/body.part").await.is_err(),
            "发布后临时对象必须立即消失"
        );
    }

    /// 🔴 **no-clobber**：正式路径已有对象时绝不覆盖——它可能正被某条已提交记录引用。
    #[tokio::test]
    async fn publishing_never_clobbers_an_existing_object() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let root = dir.path().to_string_lossy().to_string();
        write(&op, "images/1.png", b"the original bytes").await;
        write(&op, "tmp/uploads/7/aa/body.part", b"different").await;

        let out = publish_object(&op, Some(&root), "tmp/uploads/7/aa/body.part", "images/1.png")
            .await
            .expect("publish");
        assert_eq!(out, PublishOutcome::AlreadyPresent, "必须报告已存在，而不是覆盖");
        assert_eq!(
            op.read("images/1.png").await.expect("read").to_vec(),
            b"the original bytes",
            "已有对象必须原封不动"
        );
    }

    /// 恢复窗口：上次「已发布未提交」，核验一致 → 直接继续落库。
    #[tokio::test]
    async fn an_identical_published_object_verifies() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let bytes = b"same bytes as last time";
        write(&op, "images/1.png", bytes).await;

        assert!(verify_object(&op, "images/1.png", bytes.len() as u64, &digest(bytes))
            .await
            .expect("verify"));
    }

    /// 🔴 同样长度的**不同内容**必须核验失败：只比大小等于没核验。
    #[tokio::test]
    async fn same_size_different_content_fails_verification() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        write(&op, "images/1.png", b"AAAAAAAA").await;

        assert!(!verify_object(&op, "images/1.png", 8, &digest(b"BBBBBBBB"))
            .await
            .expect("verify"));
    }

    /// 正式路径上没有对象时，核验必须是 false 而不是报错。
    #[tokio::test]
    async fn a_missing_object_does_not_verify() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        assert!(!verify_object(&op, "images/nope.png", 1, &digest(b"x"))
            .await
            .expect("verify"));
    }

    /// 大于分块大小的对象也要能核验：分块读的边界必须对。
    #[tokio::test]
    async fn a_multi_chunk_object_verifies_by_streaming() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let bytes: Vec<u8> = (0..(super::VERIFY_CHUNK as usize * 2 + 12345))
            .map(|i| (i as u8).wrapping_mul(7))
            .collect();
        write(&op, "videos/big.bin", &bytes).await;

        assert!(
            verify_object(&op, "videos/big.bin", bytes.len() as u64, &digest(&bytes))
                .await
                .expect("verify"),
            "跨多个分块的对象必须核验通过"
        );
        // 少一个字节就该判不一致——尾巴确实参与了摘要。
        let mut short = bytes.clone();
        short.pop();
        assert!(
            !verify_object(&op, "videos/big.bin", bytes.len() as u64, &digest(&short))
                .await
                .expect("verify")
        );
    }

    // ---------------- 非本地后端（条件写）----------------
    //
    // 🔴 这条分支以前是「先 stat 再 copy」：stat 与 copy 之间别人可以发布同一个路径，
    // 而 `copy` 是覆盖语义——no-clobber 在这里整个被绕过去了，且当时**一条测试都没有**。
    // 现在由后端的条件写保证。fs 后端同样支持 `write_with_if_not_exists`，所以这几条
    // 用例走的就是 S3 会走的那段代码。

    #[tokio::test]
    async fn a_conditional_write_publishes_when_the_path_is_free() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        write(&op, "tmp/uploads/7/aa/body.part", b"payload bytes").await;

        let out = publish_object(&op, None, "tmp/uploads/7/aa/body.part", "files/1.bin")
            .await
            .expect("publish");
        assert_eq!(out, PublishOutcome::Published);
        assert_eq!(
            op.read("files/1.bin").await.expect("read").to_vec(),
            b"payload bytes"
        );
        assert!(
            op.stat("tmp/uploads/7/aa/body.part").await.is_err(),
            "发布后临时对象必须立即消失"
        );
    }

    /// 🔴 目标已存在：必须报告已存在，且**已有内容一个字节都不能变**。
    #[tokio::test]
    async fn a_conditional_write_never_clobbers() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        write(&op, "files/1.bin", b"the original bytes").await;
        write(&op, "tmp/uploads/7/aa/body.part", b"different").await;

        let out = publish_object(&op, None, "tmp/uploads/7/aa/body.part", "files/1.bin")
            .await
            .expect("publish");
        assert_eq!(
            out,
            PublishOutcome::AlreadyPresent,
            "必须报告已存在，而不是覆盖"
        );
        assert_eq!(
            op.read("files/1.bin").await.expect("read").to_vec(),
            b"the original bytes",
            "已有对象必须原封不动"
        );
    }

    /// 超过一个分块的对象也要能发布：条件写路径是流式搬运，不是整个读进内存。
    #[tokio::test]
    async fn a_conditional_write_streams_multiple_chunks() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let bytes: Vec<u8> = (0..(super::VERIFY_CHUNK as usize + 777))
            .map(|i| (i as u8).wrapping_mul(13))
            .collect();
        write(&op, "tmp/uploads/7/aa/body.part", &bytes).await;

        let out = publish_object(&op, None, "tmp/uploads/7/aa/body.part", "files/big.bin")
            .await
            .expect("publish");
        assert_eq!(out, PublishOutcome::Published);
        assert_eq!(op.read("files/big.bin").await.expect("read").to_vec(), bytes);
    }

    // ---------------- 跨文件系统降级 ----------------
    //
    // 📌 **能测的和不能测的说清楚**：这几条直接驱动降级函数本身（复制 → fsync →
    // no-clobber 发布 → 清理），因为在单元测试里造不出真的 `EXDEV`——那需要挂载
    // 第二个文件系统。「`EXDEV` 会路由到这里」只有那一行 match 臂，靠阅读保证；
    // 真·跨盘部署的验证属于部署演练，不属于这一层。

    #[tokio::test]
    async fn a_cross_filesystem_publish_moves_the_bytes() {
        let dir = tempfile::tempdir().expect("tmp");
        let from = dir.path().join("body.part");
        let to = dir.path().join("images/1.png");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(&from, b"cross device bytes").unwrap();

        let out = super::publish_across_filesystems(&from, &to).expect("publish");
        assert_eq!(out, PublishOutcome::Published);
        assert_eq!(std::fs::read(&to).unwrap(), b"cross device bytes");
        assert!(!from.exists(), "源临时对象要清掉");
        assert_eq!(leftover_tmp(dir.path().join("images").as_path()), 0, "中转文件不能留下");
    }

    /// 🔴 降级路径同样**绝不覆盖**：它是 no-clobber 最容易被绕过去的那条。
    #[tokio::test]
    async fn a_cross_filesystem_publish_never_clobbers() {
        let dir = tempfile::tempdir().expect("tmp");
        let from = dir.path().join("body.part");
        let to = dir.path().join("images/1.png");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::write(&to, b"the original bytes").unwrap();
        std::fs::write(&from, b"different").unwrap();

        let out = super::publish_across_filesystems(&from, &to).expect("publish");
        assert_eq!(out, PublishOutcome::AlreadyPresent);
        assert_eq!(
            std::fs::read(&to).unwrap(),
            b"the original bytes",
            "已有对象必须原封不动"
        );
        assert_eq!(leftover_tmp(dir.path().join("images").as_path()), 0, "中转文件不能留下");
    }

    /// 连续多次跨盘发布，每一份内容都原样落到自己的目标上。
    ///
    /// 📌 **它证明不了中转名唯一**：串行执行、目标各不相同，本来就撞不上。
    /// 唯一性的门禁是 [`exclusive_temps_never_hand_out_the_same_file`]（真并发）。
    /// 这条盯的是另一件事：连着发布多次，内容不会串到别人的目标上去。
    #[test]
    fn repeated_cross_filesystem_publishes_keep_their_own_bytes() {
        let dir = tempfile::tempdir().expect("tmp");
        let to = dir.path().join("images/1.png");
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        let mut seen = std::collections::HashSet::new();
        for i in 0..50 {
            let from = dir.path().join(format!("body-{i}.part"));
            std::fs::write(&from, format!("bytes {i}")).unwrap();
            let target = dir.path().join(format!("images/{i}.png"));
            super::publish_across_filesystems(&from, &target).expect("publish");
            seen.insert(std::fs::read(&target).unwrap());
        }
        assert_eq!(seen.len(), 50, "每份内容都该原样发布，说明中转没串");
    }

    /// 🔴 **真·跨挂载点**：`hard_link` 真的返回 `EXDEV`，`publish_object` 真的
    /// 路由进降级分支。
    ///
    /// 上面几条只驱动降级函数本身，证明不了「`EXDEV` 会走到那里」——那一行 match 臂
    /// 一旦写错（比如被当成普通错误报出去），它们照样全绿。这条才是判据 18a。
    ///
    /// 造法：把 operator 根下的临时目录做成指向**另一个文件系统**的符号链接。这既是
    /// 最省事的构造，也正是真实部署的形态（上传盘单独挂出来）。
    ///
    /// 需要环境变量 `PRIVCHAT_TEST_XDEV_DIR` 指向另一个挂载点上的目录。没有时**显式
    /// 报告未覆盖**并退出；设了 `PRIVCHAT_REQUIRE_XDEV_TESTS=1`（CI 门禁）则直接失败。
    /// 无论哪种，都先断言这两个路径**确实不同盘**——否则整条用例是空转。
    #[tokio::test]
    async fn a_real_cross_mount_publish_takes_the_exdev_path() {
        let Some(xdev) = std::env::var_os("PRIVCHAT_TEST_XDEV_DIR").map(std::path::PathBuf::from)
        else {
            let msg = "跨挂载点用例未覆盖：请设 PRIVCHAT_TEST_XDEV_DIR 指向另一个文件系统上的目录\
                       （macOS: diskutil eraseVolume HFS+ x $(hdiutil attach -nomount ram://65536)；\
                       Linux: /dev/shm 或第二个挂载点）";
            if std::env::var_os("PRIVCHAT_REQUIRE_XDEV_TESTS").is_some() {
                panic!("{msg}");
            }
            eprintln!("⚠️ {msg}");
            return;
        };

        let dir = tempfile::tempdir().expect("tmp");
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("images")).unwrap();

        // 临时目录挪到另一个盘上，再从 operator 根链过去。
        // 🔴 用 guard 持有，别在末尾手动删：中间任何一条断言失败都会跳过那行清理，
        // 于是每失败一次就在那个盘上留一个目录。RAM 盘只有几十兆，攒够了之后的失败
        // 与被测代码毫无关系，却要人先去趟一遍。
        // 独占创建，跟生产的中转文件、E2E 的 fixture 同一条规矩：
        // 「pid + 纳秒大概不会重」不是唯一性，异常退出或 pid 复用都能撞上残留目录。
        let far = XdevScratch(
            tempfile::Builder::new()
                .prefix("pcx-xdev-")
                .tempdir_in(&xdev)
                .expect("另一个盘上的临时目录")
                // 生命周期交给 `XdevScratch`，这里只取路径。
                .keep(),
        );
        let far = &far.0;
        std::fs::create_dir_all(far.join("uploads/7/aa")).unwrap();
        std::os::unix::fs::symlink(far, root.join("tmp")).unwrap();
        let staged = root.join("tmp/uploads/7/aa/body.part");
        std::fs::write(&staged, b"bytes from another filesystem").unwrap();

        // 🔴 先证明这两个位置**确实跨盘**。少了这一步，`PRIVCHAT_TEST_XDEV_DIR`
        // 指到同盘目录时整条用例会「通过」，而它其实什么都没验。
        let probe = root.join("images/probe.link");
        let err = std::fs::hard_link(&staged, &probe).expect_err("同盘的话这里会成功，用例即失效");
        assert_eq!(
            err.raw_os_error(),
            Some(libc::EXDEV),
            "PRIVCHAT_TEST_XDEV_DIR 必须在另一个文件系统上，实际错误：{err}"
        );

        let op = op_at(dir.path());
        let out = publish_object(
            &op,
            Some(&root.to_string_lossy()),
            "tmp/uploads/7/aa/body.part",
            "images/1.png",
        )
        .await
        .expect("跨盘发布必须成功");

        assert_eq!(out, PublishOutcome::Published);
        assert_eq!(
            std::fs::read(root.join("images/1.png")).unwrap(),
            b"bytes from another filesystem"
        );
        assert!(!staged.exists(), "源临时对象要清掉");
        assert_eq!(leftover_tmp(&root.join("images")), 0, "中转文件不能留下");

        // 同一路径再发布一次：跨盘路径同样**绝不覆盖**。
        std::fs::write(&staged, b"different").unwrap();
        let again = publish_object(
            &op,
            Some(&root.to_string_lossy()),
            "tmp/uploads/7/aa/body.part",
            "images/1.png",
        )
        .await
        .expect("publish");
        assert_eq!(again, PublishOutcome::AlreadyPresent);
        assert_eq!(
            std::fs::read(root.join("images/1.png")).unwrap(),
            b"bytes from another filesystem",
            "已有对象必须原封不动"
        );

    }

    /// 中转文件必须是**独占创建**：并发抢名字时绝不能打开同一个文件。
    ///
    /// 这条是真并发——串行跑 50 次证明不了任何事，因为串行本来就不会撞。
    #[test]
    fn exclusive_temps_never_hand_out_the_same_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let root = std::sync::Arc::new(dir.path().to_path_buf());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let root = root.clone();
            let seen = seen.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..40 {
                    let (f, p) = super::create_exclusive_temp(&root).expect("temp");
                    drop(f);
                    seen.lock().unwrap().push(p);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let paths = seen.lock().unwrap();
        let unique: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(
            unique.len(),
            paths.len(),
            "同一个中转路径被交出去两次——两个并发发布会互相截断"
        );
    }

    // ---------------- 目录创建与持久化链 ----------------

    /// 相对路径的最外层也要有人同步。
    ///
    /// 🔴 `storage/files` 里 `storage` 的 parent 是**空串**。早先按「没有父目录」跳过，
    /// 于是最外面那一级从来没落过盘。空 parent 的含义是当前目录，不是没有。
    #[test]
    fn the_outermost_level_of_a_relative_path_has_a_parent_to_sync() {
        assert_eq!(
            super::parent_to_sync(std::path::Path::new("storage")),
            Some(std::path::PathBuf::from(".")),
            "相对路径最外层的父目录是「当前目录」，不是「没有」"
        );
        assert_eq!(
            super::parent_to_sync(std::path::Path::new("storage/files")),
            Some(std::path::PathBuf::from("storage"))
        );
        assert_eq!(
            super::parent_to_sync(std::path::Path::new("/")),
            None,
            "根目录没有父目录，也不需要谁来保它"
        );
    }

    /// 多级路径要逐级建出来，每一级都在。
    #[test]
    fn creating_a_deep_path_creates_every_level() {
        let dir = tempfile::tempdir().expect("tmp");
        let deep = dir.path().join("a/b/c/d");
        super::create_dir_all_synced(&deep).expect("create");
        let mut p = dir.path().to_path_buf();
        for level in ["a", "b", "c", "d"] {
            p = p.join(level);
            assert!(p.is_dir(), "{p:?} 应当已经建出来");
        }
        // 幂等：再来一次不报错。
        super::create_dir_all_synced(&deep).expect("再来一次");
    }

    /// 记录每一次「同步了哪个目录」。
    fn recording_sync() -> (
        impl FnMut(&std::path::Path) -> std::io::Result<()>,
        std::sync::Arc<std::sync::Mutex<Vec<std::path::PathBuf>>>,
    ) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = log.clone();
        let f = move |dir: &std::path::Path| {
            sink.lock().unwrap().push(dir.to_path_buf());
            Ok(())
        };
        (f, log)
    }

    /// 🔴 **目录已经存在时，父目录照样要同步。**
    ///
    /// 「目录在」只说明有人 `mkdir` 过，**不说明那个目录项落盘了**——先建的那个进程
    /// 完全可能在 fsync 之前就崩了。早返回等于把持久化推给一个已经死掉的进程。
    ///
    /// 这条不看「目录存不存在」（那个断言删掉同步代码照样过），只看**同步有没有发生**。
    #[test]
    fn an_existing_directory_still_gets_its_parent_synced() {
        let dir = tempfile::tempdir().expect("tmp");
        let target = dir.path().join("already-there");
        // 模拟「别人抢先建好了，但还没来得及 fsync」。
        std::fs::create_dir(&target).unwrap();

        let (mut sync, log) = recording_sync();
        super::create_dir_all_with_sync(&target, &mut sync).expect("已存在不该是错误");

        assert!(
            log.lock().unwrap().contains(&dir.path().to_path_buf()),
            "目录已存在时也必须同步它的父目录，实际同步了：{:?}",
            log.lock().unwrap()
        );
    }

    /// 多级路径：**每一级**的父目录都要被同步，不是只有最里面那一级。
    #[test]
    fn every_level_of_a_new_path_gets_its_parent_synced() {
        let dir = tempfile::tempdir().expect("tmp");
        let deep = dir.path().join("a/b/c");

        let (mut sync, log) = recording_sync();
        super::create_dir_all_with_sync(&deep, &mut sync).expect("create");

        let synced = log.lock().unwrap().clone();
        for expect in [
            dir.path().to_path_buf(),
            dir.path().join("a"),
            dir.path().join("a/b"),
        ] {
            assert!(synced.contains(&expect), "{expect:?} 没被同步：{synced:?}");
        }
    }

    /// 真并发抢建：每个线程都必须看到自己那一级的父目录被同步过。
    ///
    /// 这条盯的正是「谁先建的谁负责」那个错误分工——16 个线程里只有一个会 `mkdir`
    /// 成功，其余全走 `AlreadyExists`，而它们**每一个**都得把父目录同步掉。
    #[test]
    fn every_racer_syncs_the_parent_even_though_only_one_creates() {
        let dir = tempfile::tempdir().expect("tmp");
        let target = std::sync::Arc::new(dir.path().join("contended"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let parent = dir.path().to_path_buf();

        let mut handles = Vec::new();
        for _ in 0..16 {
            let target = target.clone();
            let barrier = barrier.clone();
            let parent = parent.clone();
            handles.push(std::thread::spawn(move || {
                let (mut sync, log) = recording_sync();
                barrier.wait();
                super::create_dir_all_with_sync(&target, &mut sync).expect("create");
                assert!(
                    log.lock().unwrap().contains(&parent),
                    "抢输的那些线程也必须同步父目录"
                );
            }));
        }
        for h in handles {
            h.join().expect("线程内断言失败");
        }
        assert!(target.is_dir());
    }

    /// 同名的**文件**挡在那儿必须报错，而不是被当成建好了。
    #[test]
    fn a_file_in_the_way_is_an_error_not_a_directory() {
        let dir = tempfile::tempdir().expect("tmp");
        let target = dir.path().join("not-a-dir");
        std::fs::write(&target, b"x").unwrap();
        let err = super::create_dir_all_synced(&target).expect_err("同名文件必须报错");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    }

    /// 另一个盘上的临时目录：drop 即删。
    struct XdevScratch(std::path::PathBuf);

    impl Drop for XdevScratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// 目标目录里遗留的中转文件数。
    fn leftover_tmp(dir: &std::path::Path) -> usize {
        std::fs::read_dir(dir)
            .map(|it| {
                it.flatten()
                    .filter(|e| e.file_name().to_string_lossy().starts_with(".publish-"))
                    .count()
            })
            .unwrap_or(0)
    }

    /// 🔴 本地 link 失败（这里用「临时对象不存在」构造）必须**报错**。
    ///
    /// 以前这里会掉进「先 stat 再 copy」的降级分支。降级本身才是问题：任何一次
    /// link 失败都足以把 no-clobber 换成覆盖语义，所以现在没有降级，只有失败。
    #[tokio::test]
    async fn a_failed_link_is_an_error_not_a_fallback_copy() {
        let dir = tempfile::tempdir().expect("tmp");
        let op = op_at(dir.path());
        let root = dir.path().to_string_lossy().to_string();
        write(&op, "images/1.png", b"the original bytes").await;

        let out =
            publish_object(&op, Some(&root), "tmp/uploads/7/aa/body.part", "images/1.png").await;
        assert!(out.is_err(), "link 失败不能被当成某种成功：{out:?}");
        assert_eq!(
            op.read("images/1.png").await.expect("read").to_vec(),
            b"the original bytes",
            "失败路径上也不能碰已有对象"
        );
    }
}

