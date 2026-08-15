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

//! 上传会话目录（RESUMABLE_UPLOAD_SPEC §7）。
//!
//! 会话真源是**上传节点本地目录**：既不进 PostgreSQL（临时数据不该当账本），
//! 也不进 Redis（那样又要解决跨槽 Lua、逐出策略和租约 fencing，而这些问题在
//! 本地文件上根本不存在）。
//!
//! 组成：`state.json`（可变状态）+ `session.lock`（flock 互斥）+ `body.part`（字节）
//! + `journal`（已确认区间的追加日志）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, ServerError};

/// 这次上传走哪条路。**一经选定不可更改。**
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadMode {
    /// 尚未选择：目录刚建出来。
    Unselected,
    /// 旧的整包上传。
    Whole,
    /// 分片上传。
    Resumable,
}

/// 运行态。
///
/// 🔴 **只有 `mode` 不够**：两个并发的整包 POST 都属于 `Whole`，`mode` 拦不住它们
/// 同时进入流式接收——而今天拦住它们的是 `GETDEL`，取消一次性消费就等于把这道闸
/// 也拆了。`WholeReceiving` 就是补回来的那一道。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadStatus {
    Idle,
    /// 整包接收中：从选定模式一直持有到发布与落库结束。
    WholeReceiving,
    /// 分片上传中。
    Uploading,
    Completing,
    Completed,
    Failed,
}

/// `state.json`：**可变**状态。
///
/// 🔴 与 `manifest.json`（冻结事实）分文件。原地重写 manifest 一旦在断电时被截断，
/// 整个会话的冻结信息就全毁了、无法恢复。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionState {
    pub mode: UploadMode,
    pub status: UploadStatus,
    /// 完成后的 `file_id`；墓碑靠它回答迟到请求。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<u64>,
    /// 🔴 接收 body **之前**就分配好的 `file_id`。
    ///
    /// 崩溃重试复用它，于是「上次其实已经落库了」在提交时表现为**主键冲突**
    /// 而不是第二条记录——幂等因此不需要在正式文件表上加任何列。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved_file_id: Option<u64>,
    /// 分片上传的**总字节数**（token 里签下的那个值），选定模式时冻结。
    ///
    /// 越界的分片要能当场拒掉，`complete` 也要知道「传完了没有」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_size: Option<u64>,
    /// 🔴 **顺序分片的唯一游标**：`[0, confirmed_offset)` 的字节已经落盘且可信。
    ///
    /// 分片严格顺序提交，所以「传到哪了」是一个数,不是一组区间。之前用 append-only
    /// journal + 每段摘要 + 区间合并,是在给「乱序 + 并发分片」做持久化保证——而那个
    /// 能力我们并不要。一个游标能表达的东西,不需要一个日志文件来表达。
    #[serde(default)]
    pub confirmed_offset: u64,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            mode: UploadMode::Unselected,
            status: UploadStatus::Idle,
            file_id: None,
            reserved_file_id: None,
            total_size: None,
            confirmed_offset: 0,
        }
    }
}

/// 写一段字节的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkOutcome {
    /// 这次真的写进去了。
    Confirmed,
    /// 这段已经在游标后面了——上次写成功但响应丢了，重试就该是成功。
    AlreadyCovered,
    /// offset 越过了游标，中间会留洞。磁盘未被改动，客户端按回传的
    /// `confirmed_offset` 对齐后继续。
    Conflict,
}


/// 一个上传任务的会话目录。
pub struct UploadSession {
    dir: PathBuf,
    /// 持有它就等于持有这个会话的独占权；drop 即释放（进程被杀时由内核释放）。
    lock: File,
}

/// 模式守卫：活着代表「本次请求正占用这个会话」。
///
/// drop 时把 `status` 归位，让同一张 token 能重试。
pub struct ModeGuard<'a> {
    session: &'a UploadSession,
    /// 成功路径上置 true，drop 时就不回滚状态。
    committed: std::cell::Cell<bool>,
}

impl UploadSession {
    /// 打开（必要时创建）会话目录并取得独占锁。
    ///
    /// 🔴 目录名用 `upload_id` 而**不是 token**：token 是 bearer 凭证，会进日志、
    /// 错误信息和文件系统工具输出。`upload_id` 的字符集已在 token 验证时收死成
    /// 十六进制，拼路径是安全的。
    /// 只打开**已存在**的会话，不创建。
    ///
    /// 🔴 恢复类入口（回调 / 查状态 / complete）必须用它：`open` 会惰性建目录，
    /// 于是「会话早就没了」被伪装成「有一个空会话」，既与 `SessionGone` 的语义相反，
    /// 又在磁盘上留下垃圾目录。
    pub fn open_existing(root: &Path, uid: u64, upload_id: &str) -> Result<Option<Self>> {
        Self::guard_id(upload_id)?;
        let dir = root.join(uid.to_string()).join(upload_id);
        if !dir.is_dir() {
            return Ok(None);
        }
        Self::open(root, uid, upload_id).map(Some)
    }

    fn guard_id(upload_id: &str) -> Result<()> {
        if upload_id.is_empty()
            || upload_id.len() > 64
            || !upload_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ServerError::Validation(
                "upload_id 不是安全标识".to_string(),
            ));
        }
        Ok(())
    }

    pub fn open(root: &Path, uid: u64, upload_id: &str) -> Result<Self> {
        // 二次防线：即便上游漏了校验，也不允许可疑的 id 拼进路径。
        Self::guard_id(upload_id)?;
        let dir = root.join(uid.to_string()).join(upload_id);
        std::fs::create_dir_all(&dir)
            .map_err(|e| ServerError::Internal(format!("创建上传会话目录失败: {e}")))?;

        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(dir.join("session.lock"))
            .map_err(|e| ServerError::Internal(format!("打开会话锁失败: {e}")))?;

        Ok(Self { dir, lock })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    pub fn read_state(&self) -> Result<SessionState> {
        match std::fs::read(self.state_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| ServerError::Internal(format!("会话状态损坏: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SessionState::default()),
            Err(e) => Err(ServerError::Internal(format!("读会话状态失败: {e}"))),
        }
    }

    /// 原子替换 `state.json`：写临时文件 → fsync → rename → fsync 目录。
    ///
    /// 🔴 不能原地改写：断电时留下半截 JSON，会话就再也读不回来了。
    pub fn write_state(&self, state: &SessionState) -> Result<()> {
        let tmp = self.dir.join("state.tmp");
        let bytes = serde_json::to_vec(state)
            .map_err(|e| ServerError::Internal(format!("序列化会话状态失败: {e}")))?;
        {
            let mut f = File::create(&tmp)
                .map_err(|e| ServerError::Internal(format!("写会话状态失败: {e}")))?;
            f.write_all(&bytes)
                .map_err(|e| ServerError::Internal(format!("写会话状态失败: {e}")))?;
            f.sync_all()
                .map_err(|e| ServerError::Internal(format!("同步会话状态失败: {e}")))?;
        }
        std::fs::rename(&tmp, self.state_path())
            .map_err(|e| ServerError::Internal(format!("替换会话状态失败: {e}")))?;
        // 目录项本身也要落盘，否则 rename 可能在崩溃后丢失。
        if let Ok(d) = File::open(&self.dir) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    /// 直接把会话标成已完成（用于「发现正式记录已存在」时补写墓碑）。
    pub fn mark_completed(&self, file_id: u64) -> Result<()> {
        let mut st = self.read_state()?;
        st.status = UploadStatus::Completed;
        st.file_id = Some(file_id);
        self.write_state(&st)
    }

    /// 读出预留的 `file_id`（若有）。
    pub fn reserved_file_id(&self) -> Result<Option<u64>> {
        Ok(self.read_state()?.reserved_file_id)
    }

    /// 记下这次上传要用的 `file_id`。落盘后才去接收 body。
    pub fn reserve_file_id(&self, file_id: u64) -> Result<()> {
        let mut st = self.read_state()?;
        st.reserved_file_id = Some(file_id);
        self.write_state(&st)
    }

    /// 已完成的话给出 `file_id`（墓碑）。
    ///
    /// 迟到的重复请求靠它拿到**成功**结果，而不是「这次上传已完成」这种错误——
    /// 对客户端来说那和失败无法区分。
    pub fn completed_file_id(&self) -> Result<Option<u64>> {
        let st = self.read_state()?;
        Ok(if st.status == UploadStatus::Completed {
            st.file_id
        } else {
            None
        })
    }

    /// 非阻塞独占锁；拿不到返回 `false`（清理任务用）。
    pub fn try_lock_exclusive(&self) -> Result<bool> {
        match lock_impl(&self.lock, true) {
            Ok(()) => Ok(true),
            Err(ServerError::Validation(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// 选定 / 校验模式为 `Whole`，并占住运行态。
    ///
    /// 返回的守卫活着期间，同一 `upload_id` 上的另一个整包 POST 会拿到
    /// `UploadInProgress`，分片请求会拿到 `UploadModeConflict`。
    pub fn begin_whole(&self) -> Result<ModeGuard<'_>> {
        // 🔴 **非阻塞**。撞上另一个正在进行的上传时，正确行为是立刻告诉调用方
        // 「正在进行中」，而不是把这个 HTTP 请求挂在锁上等到超时——那既占着连接，
        // 又让客户端拿不到可判断的错误。
        if !self.try_lock_exclusive()? {
            return Err(ServerError::Validation(
                "同一份上传正在进行中".to_string(),
            ));
        }
        let mut state = self.read_state()?;

        match (state.mode, state.status) {
            // 已完成：墓碑负责回答，不要重新开始上传。
            (_, UploadStatus::Completed) => {
                return Err(ServerError::Validation(format!(
                    "该上传已完成（file_id={:?}）",
                    state.file_id
                )));
            }
            (UploadMode::Resumable, _) => {
                return Err(ServerError::Validation(
                    "该 token 已用于分片上传，不能再走整包".to_string(),
                ));
            }
            // 🔴 **拿到锁 + 状态是 WholeReceiving = 上一个进程崩了。**
            //
            // 真正并发的请求根本走不到这里：它在上面的 `try_lock` 就失败了。
            // 能拿到锁却看见「接收中」，只能说明持锁进程已经死亡、内核释放了 flock，
            // 而磁盘上的状态没来得及回滚（`Drop` 不会在 SIGKILL 时执行）。
            //
            // 把它当成「仍在上传」永久拒绝，等于让一次崩溃把这个上传**永久锁死**——
            // 而「进程崩了还能接着传」正是断点续传的核心场景。
            (_, UploadStatus::WholeReceiving) => {
                tracing::warn!(
                    "🔧 会话 {:?} 停在 WholeReceiving 但锁是空的：按崩溃恢复处理",
                    self.dir
                );
            }
            _ => {}
        }

        state.mode = UploadMode::Whole;
        state.status = UploadStatus::WholeReceiving;
        self.write_state(&state)?;

        Ok(ModeGuard {
            session: self,
            committed: std::cell::Cell::new(false),
        })
    }

    /// 分片模式：选定 `Resumable` 并占住会话。
    ///
    /// `total_size` 来自 token（`sealed_blob_size`），会话第一次选定模式时冻结。
    pub fn begin_resumable(&self, total_size: u64) -> Result<ModeGuard<'_>> {
        if !self.try_lock_exclusive()? {
            return Err(ServerError::Validation(
                "同一份上传正在进行中".to_string(),
            ));
        }
        let mut state = self.read_state()?;
        match (state.mode, state.status) {
            (_, UploadStatus::Completed) => {
                return Err(ServerError::Validation(format!(
                    "该上传已完成（file_id={:?}）",
                    state.file_id
                )));
            }
            // 🔴 整包与分片互斥：同一张 token 只能走一条路。
            (UploadMode::Whole, _) => {
                return Err(ServerError::Validation(
                    "该 token 已用于整包上传，不能再走分片".to_string(),
                ));
            }
            // 拿到锁却看见「上传中」= 上一个进程崩了（真并发在 try_lock 就挡住了）。
            // 分片上传本来就是为崩溃准备的，这里当然要接着传。
            (_, UploadStatus::Uploading) => {}
            _ => {}
        }
        if let Some(frozen) = state.total_size {
            if frozen != total_size {
                return Err(ServerError::Validation(format!(
                    "该会话已冻结总大小 {frozen}，与本次声明的 {total_size} 不符"
                )));
            }
        }
        state.mode = UploadMode::Resumable;
        state.status = UploadStatus::Uploading;
        state.total_size = Some(total_size);
        self.write_state(&state)?;
        Ok(ModeGuard {
            session: self,
            committed: std::cell::Cell::new(false),
        })
    }

    pub fn body_path(&self) -> PathBuf {
        self.dir.join("body.part")
    }

    /// 已确认区间。顺序游标之下永远是「一段连续的前缀」。
    ///
    /// 保留数组形态是为了不动线上协议;真源是 `confirmed_offset`。
    pub fn confirmed_ranges(&self) -> Result<Vec<(u64, u64)>> {
        let at = self.read_state()?.confirmed_offset;
        Ok(if at == 0 { Vec::new() } else { vec![(0, at)] })
    }

    /// 已确认的总字节数 == 游标。
    pub fn confirmed_bytes(&self) -> Result<u64> {
        Ok(self.read_state()?.confirmed_offset)
    }

    /// 顺序写入一段字节。
    ///
    /// 只接受 `offset == confirmed_offset`:
    /// - 落后(`offset < 游标`)= 上次写成功但响应丢了,回 `AlreadyCovered`;
    /// - 超前(`offset > 游标`)= 中间会留洞,回 `Conflict`,由调用方把当前游标带回给
    ///   客户端。客户端不需要自己记进度,服务端说了算。
    ///
    /// 🔴 顺序:pwrite → `fdatasync(body.part)` → 原子更新游标。
    /// 反过来的话,崩在中间就成了「游标说确认了、字节没落盘」——客户端不会重传,
    /// 却要等到 complete 校验摘要时才发现,是最难查的一种坏。按这个顺序,崩溃的
    /// 最坏后果只是这一片重传一次。
    pub fn write_chunk(&self, offset: u64, bytes: &[u8]) -> Result<ChunkOutcome> {
        if bytes.is_empty() {
            return Err(ServerError::Validation("分片不能为空".to_string()));
        }
        let mut state = self.read_state()?;
        let total = state
            .total_size
            .ok_or_else(|| ServerError::Validation("会话尚未选定分片模式".to_string()))?;
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| ServerError::Validation("分片区间溢出".to_string()))?;
        if end > total {
            return Err(ServerError::Validation(format!(
                "分片区间 [{offset}, {end}) 越过总大小 {total}"
            )));
        }

        if offset < state.confirmed_offset {
            // 整段都在游标之内 = 纯重试。跨越游标说明客户端的分片边界变了,
            // 让它按游标重新对齐,不要就着旧边界往里写。
            return Ok(if end <= state.confirmed_offset {
                ChunkOutcome::AlreadyCovered
            } else {
                ChunkOutcome::Conflict
            });
        }
        if offset > state.confirmed_offset {
            return Ok(ChunkOutcome::Conflict);
        }

        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(self.body_path())
            .map_err(|e| ServerError::Internal(format!("打开 body.part 失败: {e}")))?;
        {
            use std::os::unix::fs::FileExt;
            file.write_all_at(bytes, offset)
                .map_err(|e| ServerError::Internal(format!("写分片失败: {e}")))?;
        }
        file.sync_data()
            .map_err(|e| ServerError::Internal(format!("同步 body.part 失败: {e}")))?;

        state.confirmed_offset = end;
        self.write_state(&state)?;
        Ok(ChunkOutcome::Confirmed)
    }

    /// 传完了吗。
    pub fn is_complete(&self) -> Result<bool> {
        let state = self.read_state()?;
        let Some(total) = state.total_size else {
            return Ok(false);
        };
        Ok(state.confirmed_offset == total)
    }

    /// 丢弃整个会话目录。
    pub fn discard(self) -> Result<()> {
        let dir = self.dir.clone();
        drop(self);
        std::fs::remove_dir_all(&dir)
            .map_err(|e| ServerError::Internal(format!("删除会话目录 {dir:?} 失败: {e}")))
    }
}

impl ModeGuard<'_> {
    /// 上传成功：记下 `file_id`，状态转 `Completed`（墓碑）。
    pub fn complete(self, file_id: u64) -> Result<()> {
        let mut state = self.session.read_state()?;
        state.status = UploadStatus::Completed;
        state.file_id = Some(file_id);
        self.session.write_state(&state)?;
        self.committed.set(true);
        Ok(())
    }
}

impl Drop for ModeGuard<'_> {
    /// 失败路径：把状态放回 `Idle`，同一张 token 还能重试。
    ///
    /// 不回滚 `mode`——模式一经选定就不该改变，重试仍走整包。
    fn drop(&mut self) {
        if self.committed.get() {
            return;
        }
        if let Ok(mut state) = self.session.read_state() {
            if state.status == UploadStatus::WholeReceiving {
                state.status = UploadStatus::Idle;
                let _ = self.session.write_state(&state);
            }
        }
    }
}

#[cfg(unix)]
fn lock_impl(file: &File, non_blocking: bool) -> Result<()> {
    use std::os::unix::io::AsRawFd;
    let op = libc::LOCK_EX | if non_blocking { libc::LOCK_NB } else { 0 };
    // SAFETY: fd 来自一个活着的 File，flock 不会转移所有权。
    let rc = unsafe { libc::flock(file.as_raw_fd(), op) };
    if rc == 0 {
        return Ok(());
    }
    let err = std::io::Error::last_os_error();
    if non_blocking
        && matches!(
            err.raw_os_error(),
            Some(libc::EWOULDBLOCK) | Some(libc::EINTR)
        )
    {
        // 约定：非阻塞拿不到锁用 Validation 表达，由 try_lock_exclusive 翻成 false。
        return Err(ServerError::Validation("locked".to_string()));
    }
    Err(ServerError::Internal(format!("flock 失败: {err}")))
}

#[cfg(not(unix))]
fn lock_impl(_file: &File, _non_blocking: bool) -> Result<()> {
    // 非 Unix 平台不在部署范围内；不静默放行，免得「没有锁」被当成「拿到锁」。
    Err(ServerError::Internal(
        "上传会话锁仅支持 Unix".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn a_fresh_session_starts_unselected() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        let st = s.read_state().expect("state");
        assert_eq!(st.mode, UploadMode::Unselected);
        assert_eq!(st.status, UploadStatus::Idle);
    }

    /// 🔴 这是 `GETDEL` 拿掉之后真正打开的那个口子：两个整包请求都属于 `Whole`，
    /// 只靠 `mode` 拦不住。
    #[test]
    fn a_second_whole_upload_cannot_start_while_one_is_receiving() {
        let r = root();
        let a = UploadSession::open(r.path(), 7, "abc123").expect("open");
        let _guard = a.begin_whole().expect("first");

        let b = UploadSession::open(r.path(), 7, "abc123").expect("open");
        assert!(
            b.begin_whole().is_err(),
            "并发的第二个整包上传必须被拒绝"
        );
    }

    /// 失败后状态要放回去，否则同一张 token 永远重试不了。
    #[test]
    fn a_failed_upload_frees_the_session_for_a_retry() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        {
            let _guard = s.begin_whole().expect("first");
            // guard 在这里 drop —— 模拟失败退出
        }
        let st = s.read_state().expect("state");
        assert_eq!(st.status, UploadStatus::Idle);
        assert_eq!(st.mode, UploadMode::Whole, "模式一经选定不回滚");

        s.begin_whole().expect("重试必须可以开始");
    }

    /// 完成后再来的请求由墓碑回答，不能重新开始一次上传。
    #[test]
    fn a_completed_session_refuses_to_start_again() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        s.begin_whole().expect("first").complete(4242).expect("complete");

        let st = s.read_state().expect("state");
        assert_eq!(st.status, UploadStatus::Completed);
        assert_eq!(st.file_id, Some(4242));
        assert!(s.begin_whole().is_err());
    }

    /// 模式互斥：走过整包的会话不能再被当成分片会话（反之亦然）。
    #[test]
    fn a_whole_session_cannot_be_reused_for_chunks() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        {
            let _g = s.begin_whole().expect("whole");
        }
        let mut st = s.read_state().expect("state");
        st.mode = UploadMode::Resumable; // 冒充分片会话
        s.write_state(&st).expect("write");

        assert!(
            s.begin_whole().is_err(),
            "已选定分片的会话不能再走整包"
        );
    }

    /// 🔴 预留的 `file_id` 必须跨请求存活：崩溃重试要复用它，落库时才会撞主键
    /// 而不是产生第二条文件记录。幂等因此不需要在业务库上加任何列。
    #[test]
    fn a_reserved_file_id_survives_for_the_retry() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        assert_eq!(s.reserved_file_id().expect("read"), None);

        {
            let _g = s.begin_whole().expect("first");
            s.reserve_file_id(90210).expect("reserve");
            // guard drop = 这次失败了
        }

        // 新的一次请求（新的会话对象，模拟新进程）读到同一个预留 id。
        let again = UploadSession::open(r.path(), 7, "abc123").expect("reopen");
        assert_eq!(again.reserved_file_id().expect("read"), Some(90210));
        assert_eq!(again.read_state().expect("state").status, UploadStatus::Idle);
    }

    /// 会话没了就是没了：这里不做任何「从业务库恢复会话」的尝试。
    #[test]
    fn a_lost_session_starts_over_rather_than_recovering() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        s.begin_whole().expect("first").complete(4242).expect("complete");
        assert_eq!(s.completed_file_id().expect("read"), Some(4242));

        // 清理任务删掉目录后：状态回到「什么都没有」，而不是去别处找回来。
        std::fs::remove_dir_all(s.dir()).expect("rm");
        let fresh = UploadSession::open(r.path(), 7, "abc123").expect("reopen");
        assert_eq!(fresh.completed_file_id().expect("read"), None);
        assert_eq!(fresh.reserved_file_id().expect("read"), None);
    }

    /// 🔴 持锁进程死亡后必须能接着传——把残留的 `WholeReceiving` 当成「仍在进行中」
    /// 就是让一次崩溃永久锁死这个上传。
    ///
    /// 📌 **这条测试证明到哪一步，说清楚**：它**真的**取得了 flock（`begin_whole`），
    /// 然后 `mem::forget` 掉守卫让 `Drop` 不回滚状态（模拟 SIGKILL 跳过析构），
    /// 再让会话对象析构——fd 关闭，内核释放锁，与进程死亡时的释放路径**是同一条**。
    ///
    /// 它**没有**覆盖的：真正跨进程的 SIGKILL。要那个还需要一个子进程持锁再被杀，
    /// 属于尚未补的缺口，不要因为这条绿了就以为已经验过。
    #[test]
    fn a_session_left_receiving_after_the_lock_died_can_be_resumed() {
        let r = root();

        {
            let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
            let guard = s.begin_whole().expect("take the lock for real");
            s.reserve_file_id(31337).expect("reserve");
            // 跳过 Drop：状态停在 WholeReceiving，就像进程被 SIGKILL。
            std::mem::forget(guard);
            // s 在这里析构 → fd 关闭 → 内核释放 flock。
        }

        let again = UploadSession::open(r.path(), 7, "abc123").expect("reopen");
        assert_eq!(
            again.read_state().expect("state").status,
            UploadStatus::WholeReceiving,
            "前置条件：磁盘上确实留着 WholeReceiving"
        );
        assert_eq!(again.reserved_file_id().expect("read"), Some(31337));
        let _guard = again
            .begin_whole()
            .expect("锁已释放 + 残留 WholeReceiving 必须可恢复，而不是永久拒绝");
    }

    /// 与上一条的区别：**锁还被人拿着**时，第二个请求仍然必须被挡住。
    #[test]
    fn a_live_upload_still_blocks_a_second_one() {
        let r = root();
        let a = UploadSession::open(r.path(), 7, "abc123").expect("open");
        let _held = a.begin_whole().expect("first");

        let b = UploadSession::open(r.path(), 7, "abc123").expect("open");
        assert!(b.begin_whole().is_err(), "活着的上传必须挡住第二个");
    }

    /// 🔴 恢复类入口不得惰性建目录：会话没了就该说没了，而不是造一个空的出来，
    /// 更不该在磁盘上留垃圾。
    #[test]
    fn opening_a_missing_session_for_recovery_creates_nothing() {
        let r = root();
        let got = UploadSession::open_existing(r.path(), 7, "abc123").expect("call");
        assert!(got.is_none(), "不存在的会话必须报告不存在");
        assert!(
            !r.path().join("7").join("abc123").exists(),
            "不得因为查询而建出目录"
        );

        // 正常创建之后就能打开了。
        let _s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        assert!(UploadSession::open_existing(r.path(), 7, "abc123")
            .expect("call")
            .is_some());
    }

    /// 补墓碑：发现正式记录已存在时，不经过 guard 也要能把会话标成已完成。
    #[test]
    fn a_session_can_be_marked_completed_when_the_row_already_exists() {
        let r = root();
        let s = UploadSession::open(r.path(), 7, "abc123").expect("open");
        s.reserve_file_id(555).expect("reserve");
        s.mark_completed(555).expect("mark");
        assert_eq!(s.completed_file_id().expect("read"), Some(555));
        assert!(s.begin_whole().is_err(), "已完成的会话不能再开始");
    }

    /// `upload_id` 直接进路径，可疑值必须在这一层也拦住（纵深防御）。
    #[test]
    fn an_unsafe_upload_id_never_reaches_the_filesystem() {
        let r = root();
        for evil in ["../escape", "a/b", "", "名字", "zz"] {
            assert!(
                UploadSession::open(r.path(), 7, evil).is_err(),
                "upload_id {evil:?} 必须被拒"
            );
        }
    }

    /// 🔴 崩溃窗口：字节已 fsync 落盘、游标还没更新时进程死掉。
    ///
    /// 这是删掉 journal 之后**唯一**新增的风险面。旧实现里"确认"这件事记在 journal
    /// 里，现在记在 `state.json` 的游标上，于是 `pwrite → fdatasync → 更新游标` 这三
    /// 步之间多了一个可以死人的缝。
    ///
    /// 必须保证死在缝里是**安全**的那一侧：游标没动 → status 仍报旧值 → 客户端重传
    /// 这一段。代价是白传一片，而不是"游标说传完了、字节其实没落盘"——后者要等到
    /// complete 校验摘要才暴露，是最难查的一种坏。
    ///
    /// 这里直接构造那个中间态（写完不更新游标），因为游标只存在于 `state.json`，
    /// 没有任何内存态参与——杀不杀进程，能观察到的东西完全一样。
    #[test]
    fn bytes_on_disk_without_a_cursor_bump_are_not_counted_as_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let s = UploadSession::open(root.path(), 7, "c7a5f0").unwrap();
        let _g = s.begin_resumable(8).unwrap();

        s.write_chunk(0, b"aaaa").unwrap();
        assert_eq!(s.confirmed_bytes().unwrap(), 4);

        // ---- 崩溃窗口：字节落盘，游标未更新 ----
        {
            use std::os::unix::fs::FileExt;
            let f = OpenOptions::new()
                .write(true)
                .truncate(false)
                .open(s.body_path())
                .unwrap();
            f.write_all_at(b"bbbb", 4).unwrap();
            f.sync_data().unwrap();
        }

        // 重启后的视角：那 4 个字节在盘上，但**不算数**。
        let again = UploadSession::open(root.path(), 7, "c7a5f0").unwrap();
        assert_eq!(
            again.confirmed_bytes().unwrap(),
            4,
            "没更新游标的字节被当成已确认了——客户端不会重传，文件会缺一段"
        );
        assert!(!again.is_complete().unwrap());

        // 客户端按 status 重传该段（可以换更小的分片），一切照常。
        assert_eq!(
            again.write_chunk(4, b"bb").unwrap(),
            ChunkOutcome::Confirmed
        );
        assert_eq!(
            again.write_chunk(6, b"bb").unwrap(),
            ChunkOutcome::Confirmed
        );
        assert!(again.is_complete().unwrap());
        assert_eq!(std::fs::read(again.body_path()).unwrap(), b"aaaabbbb");
    }
}
