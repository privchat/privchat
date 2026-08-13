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
//! 本批只落地**模式锁**所需的最小集合：`state.json` + `flock`。区间上传要用的
//! `body.part` / bitmap / journal 在下一批。

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
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            mode: UploadMode::Unselected,
            status: UploadStatus::Idle,
            file_id: None,
        }
    }
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
    pub fn open(root: &Path, uid: u64, upload_id: &str) -> Result<Self> {
        // 二次防线：即便上游漏了校验，也不允许可疑的 id 拼进路径。
        if upload_id.is_empty()
            || upload_id.len() > 64
            || !upload_id.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Err(ServerError::Validation(
                "upload_id 不是安全标识".to_string(),
            ));
        }
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
            (_, UploadStatus::WholeReceiving) => {
                return Err(ServerError::Validation(
                    "同一份上传正在进行中".to_string(),
                ));
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
}
