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
use chrono::{DateTime, Local, NaiveDate};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing_subscriber::{
    fmt::{self, writer::MakeWriter},
    layer::SubscriberExt,
    util::SubscriberInitExt,
    EnvFilter,
};

/// 归档日志保留天数的默认值。
///
/// 生产上没有这个限制的后果实测过：日志按天归档但从不删除，`server.log.*` 攒到 **40.5GB**，
/// 占了已用磁盘的一半以上。按每天 4–6GB 的写入速度，磁盘撑满只是时间问题。
pub const DEFAULT_LOG_RETENTION_DAYS: u32 = 7;

#[derive(Debug)]
struct DailyRenameState {
    dir: PathBuf,
    filename: String,
    current_date: NaiveDate,
    file: File,
    /// 0 = 不清理（留给「我就是要全部留着」的场景，例如取证）
    retention_days: u32,
}

#[derive(Clone, Debug)]
struct DailyRenameAppender {
    state: Arc<Mutex<DailyRenameState>>,
}

#[derive(Clone, Debug)]
struct DailyRenameWriter {
    state: Arc<Mutex<DailyRenameState>>,
}

impl DailyRenameAppender {
    fn new(path: &Path, retention_days: u32) -> Result<Self> {
        let dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let filename = path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("server.log"))
            .to_string_lossy()
            .to_string();

        fs::create_dir_all(&dir).with_context(|| format!("创建日志目录失败: {}", dir.display()))?;

        rotate_stale_base_log(&dir, &filename)?;

        let base_path = dir.join(&filename);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&base_path)
            .with_context(|| format!("打开日志文件失败: {}", base_path.display()))?;

        // 启动时先扫一遍：进程可能停了很久，期间没人清理过。
        purge_expired_archives(&dir, &filename, retention_days, Local::now().date_naive());

        let state = DailyRenameState {
            dir,
            filename,
            current_date: Local::now().date_naive(),
            file,
            retention_days,
        };

        Ok(Self {
            state: Arc::new(Mutex::new(state)),
        })
    }
}

impl<'a> MakeWriter<'a> for DailyRenameAppender {
    type Writer = DailyRenameWriter;

    fn make_writer(&'a self) -> Self::Writer {
        DailyRenameWriter {
            state: Arc::clone(&self.state),
        }
    }
}

impl Write for DailyRenameWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| std::io::Error::other("log state mutex poisoned"))?;

        rotate_if_day_changed(&mut state)?;
        state.file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| std::io::Error::other("log state mutex poisoned"))?;
        state.file.flush()
    }
}

fn rotate_if_day_changed(state: &mut DailyRenameState) -> std::io::Result<()> {
    let today = Local::now().date_naive();
    if today == state.current_date {
        return Ok(());
    }

    state.file.flush()?;

    let base_path = state.dir.join(&state.filename);
    if base_path.exists() {
        let archive_path = next_archive_path(&state.dir, &state.filename, state.current_date);
        fs::rename(&base_path, &archive_path)?;
    }

    state.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&base_path)?;
    state.current_date = today;

    // 归档刚产生，正是清旧的时机。清理失败不能影响写日志——磁盘满了写不进去是一回事，
    // 因为删不掉旧文件就把当前这条日志也丢了是另一回事。
    purge_expired_archives(&state.dir, &state.filename, state.retention_days, today);
    Ok(())
}

/// 删除超过保留期的归档日志（`server.log.YYYY-MM-DD` 及其 `.N` 变体）。
///
/// 判据取**文件名里的日期**而不是 mtime：归档文件写完就不再改动，mtime 等价于归档日；但
/// 备份、复制、rsync 都会把 mtime 刷新成当下，那时按 mtime 判断会把该删的留下来。文件名
/// 是归档时自己写的，不会被这些操作改掉。
///
/// 只认自己的命名规则，不匹配的文件一律不碰——同目录下可能有别人的东西，日志清理没有理由
/// 删一个自己不认识的文件。
///
/// 语义是**保留最近 `retention_days` 天，含今天**：7 天 = 今天 + 往前 6 个归档，第 7 天前的
/// 归档删掉。窗口含今天这点要说死，不然「保留 7 天」到底留 7 个还是 8 个文件，每个人的读法
/// 都不一样。
fn purge_expired_archives(dir: &Path, filename: &str, retention_days: u32, today: NaiveDate) {
    if retention_days == 0 {
        return;
    }
    let cutoff = match today.checked_sub_days(chrono::Days::new((retention_days - 1) as u64)) {
        Some(date) => date,
        None => return,
    };

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("清理归档日志失败（读取目录 {}）: {e}", dir.display());
            return;
        }
    };

    let prefix = format!("{filename}.");
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        // rest 形如 `2026-08-01` 或 `2026-08-01.3`
        let date_part = rest.split('.').next().unwrap_or("");
        let Ok(archived) = NaiveDate::parse_from_str(date_part, "%Y-%m-%d") else {
            continue;
        };
        if archived >= cutoff {
            continue;
        }
        match fs::remove_file(entry.path()) {
            Ok(()) => println!("清理过期日志: {}", entry.path().display()),
            Err(e) => eprintln!("清理过期日志失败 {}: {e}", entry.path().display()),
        }
    }
}

fn rotate_stale_base_log(dir: &Path, filename: &str) -> Result<()> {
    let base_path = dir.join(filename);
    if !base_path.exists() {
        return Ok(());
    }

    let metadata = fs::metadata(&base_path)
        .with_context(|| format!("读取日志文件元信息失败: {}", base_path.display()))?;
    let modified = metadata
        .modified()
        .with_context(|| format!("读取日志文件修改时间失败: {}", base_path.display()))?;
    let modified_date: NaiveDate = DateTime::<Local>::from(modified).date_naive();
    let today = Local::now().date_naive();

    if modified_date < today {
        let archive_path = next_archive_path(dir, filename, modified_date);
        fs::rename(&base_path, &archive_path).with_context(|| {
            format!(
                "重命名历史日志失败: {} -> {}",
                base_path.display(),
                archive_path.display()
            )
        })?;
    }

    Ok(())
}

fn next_archive_path(dir: &Path, filename: &str, date: NaiveDate) -> PathBuf {
    let date_str = date.format("%Y-%m-%d").to_string();
    let first = dir.join(format!("{filename}.{date_str}"));
    if !first.exists() {
        return first;
    }

    let mut idx: u32 = 1;
    loop {
        let candidate = dir.join(format!("{filename}.{date_str}.{idx}"));
        if !candidate.exists() {
            return candidate;
        }
        idx = idx.saturating_add(1);
    }
}

/// 初始化日志系统
///
/// - `log_file` 为 None 时只输出到 stdout
/// - `log_file` 指定路径时，同时输出到 stdout 和文件
///   当前日志固定写入 `server.log`，跨天后自动重命名为 `server.log.YYYY-MM-DD`
///   例如 `--log-file /data/logs/privchat/server.log`
///   会保持当前文件为 `server.log`，并在下一天归档为 `server.log.2026-02-18`
pub fn init_logging(
    log_level: &str,
    log_format: Option<&str>,
    log_file: Option<&str>,
    quiet: bool,
    retention_days: u32,
) -> Result<()> {
    let level = if quiet { "error" } else { log_level };
    // 默认将 msgtrans 传输层日志设为 info，避免大量底层 debug 日志刷屏
    let default_filter = format!("{},msgtrans=info", level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&default_filter));

    if let Some(path) = log_file {
        let file_appender = DailyRenameAppender::new(Path::new(path), retention_days)?;

        let file_layer = fmt::layer().with_ansi(false).with_writer(file_appender);

        let stdout_layer = fmt::layer().compact();

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    } else {
        match log_format {
            Some("json") => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().json())
                    .init();
            }
            Some("pretty") | Some("dev") => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().pretty())
                    .init();
            }
            _ => {
                tracing_subscriber::registry()
                    .with(env_filter)
                    .with(fmt::layer().compact())
                    .init();
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"x").expect("write fixture");
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        v.sort();
        v
    }

    /// 保留期内的留下，超期的删掉。当前正在写的 `server.log` 没有日期后缀，永远不参与清理。
    #[test]
    fn expired_archives_go_and_recent_ones_stay() {
        let dir = std::env::temp_dir().join(format!("privchat-log-purge-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");

        let today = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        touch(&dir, "server.log");
        touch(&dir, "server.log.2026-08-06"); // 1 天前
        touch(&dir, "server.log.2026-08-01"); // 窗口最边缘：today-6，保留
        touch(&dir, "server.log.2026-07-31"); // 出窗口第一天，删
        touch(&dir, "server.log.2026-07-20"); // 远超期

        purge_expired_archives(&dir, "server.log", 7, today);

        assert_eq!(
            names(&dir),
            vec![
                "server.log".to_string(),
                "server.log.2026-08-01".to_string(),
                "server.log.2026-08-06".to_string(),
            ],
            "保留期是 [today-7, today]，7 天前那份应该被清掉，当前 server.log 不能动",
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 同一天轮转多次会产生 `.1` `.2` 后缀，它们同样要按日期判定。
    #[test]
    fn indexed_archives_are_purged_by_their_date() {
        let dir = std::env::temp_dir().join(format!("privchat-log-idx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");

        let today = NaiveDate::from_ymd_opt(2026, 8, 7).unwrap();
        touch(&dir, "server.log.2026-07-01.1");
        touch(&dir, "server.log.2026-07-01.2");
        touch(&dir, "server.log.2026-08-06.1");

        purge_expired_archives(&dir, "server.log", 7, today);

        assert_eq!(names(&dir), vec!["server.log.2026-08-06.1".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// 不认识的文件一个都不许碰——同目录下可能放着别人的东西。
    #[test]
    fn foreign_files_are_never_touched() {
        let dir = std::env::temp_dir().join(format!("privchat-log-foreign-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");

        touch(&dir, "server.log.2026-07-01"); // 该删
        touch(&dir, "access.log.2026-07-01"); // 别的前缀
        touch(&dir, "server.log.backup"); // 日期解析不出来
        touch(&dir, "important.tar.gz");

        purge_expired_archives(
            &dir,
            "server.log",
            7,
            NaiveDate::from_ymd_opt(2026, 8, 7).unwrap(),
        );

        assert_eq!(
            names(&dir),
            vec![
                "access.log.2026-07-01".to_string(),
                "important.tar.gz".to_string(),
                "server.log.backup".to_string(),
            ],
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// 0 = 关掉清理。取证场景需要留全量，不能因为默认值就把证据删了。
    #[test]
    fn zero_retention_disables_purging() {
        let dir = std::env::temp_dir().join(format!("privchat-log-zero-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("mkdir");

        touch(&dir, "server.log.2020-01-01");
        purge_expired_archives(
            &dir,
            "server.log",
            0,
            NaiveDate::from_ymd_opt(2026, 8, 7).unwrap(),
        );

        assert_eq!(names(&dir), vec!["server.log.2020-01-01".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }
}
