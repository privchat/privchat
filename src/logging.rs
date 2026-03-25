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

#[derive(Debug)]
struct DailyRenameState {
    dir: PathBuf,
    filename: String,
    current_date: NaiveDate,
    file: File,
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
    fn new(path: &Path) -> Result<Self> {
        let dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let filename = path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("server.log"))
            .to_string_lossy()
            .to_string();

        fs::create_dir_all(&dir)
            .with_context(|| format!("创建日志目录失败: {}", dir.display()))?;

        rotate_stale_base_log(&dir, &filename)?;

        let base_path = dir.join(&filename);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&base_path)
            .with_context(|| format!("打开日志文件失败: {}", base_path.display()))?;

        let state = DailyRenameState {
            dir,
            filename,
            current_date: Local::now().date_naive(),
            file,
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
    Ok(())
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
) -> Result<()> {
    let level = if quiet { "error" } else { log_level };
    // 默认将 msgtrans 传输层日志设为 info，避免大量底层 debug 日志刷屏
    let default_filter = format!("{},msgtrans=info", level);
    let env_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&default_filter));

    if let Some(path) = log_file {
        let file_appender = DailyRenameAppender::new(Path::new(path))?;

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
