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

use clap::{Parser, Subcommand};

// 确保 Parser trait 被使用
impl Cli {
    /// 解析命令行参数
    pub fn parse() -> Self {
        <Self as Parser>::parse()
    }
}

/// PrivChat Server - 高性能即时消息服务器
#[derive(Parser, Debug)]
#[command(name = "privchat-server")]
#[command(version)]
#[command(about = "基于 msgtrans 框架的高性能聊天服务器", long_about = None)]
pub struct Cli {
    /// 配置文件路径
    #[arg(long, value_name = "FILE", help = "指定配置文件路径")]
    pub config_file: Option<String>,

    /// 运行环境
    #[arg(
        long,
        value_name = "ENV",
        help = "运行环境: production, development, test"
    )]
    pub env: Option<String>,

    /// 服务器监听地址
    #[arg(long, value_name = "ADDRESS", help = "服务器监听地址")]
    pub host: Option<String>,

    /// TCP 端口
    #[arg(long, value_name = "PORT", help = "TCP 协议端口")]
    pub tcp_port: Option<u16>,

    /// WebSocket 端口
    #[arg(long, value_name = "PORT", help = "WebSocket 协议端口")]
    pub ws_port: Option<u16>,

    /// QUIC 端口
    #[arg(long, value_name = "PORT", help = "QUIC 协议端口")]
    pub quic_port: Option<u16>,

    /// 最大连接数
    #[arg(long, value_name = "NUM", help = "最大并发连接数")]
    pub max_connections: Option<u32>,

    /// 工作线程数（0 = 自动）
    #[arg(long, value_name = "NUM", help = "工作线程数（0 表示自动）")]
    pub worker_threads: Option<u32>,

    /// 日志级别
    #[arg(
        long,
        value_name = "LEVEL",
        help = "日志级别: trace, debug, info, warn, error"
    )]
    pub log_level: Option<String>,

    /// 日志格式
    #[arg(long, value_name = "FORMAT", help = "日志格式: pretty, json, compact")]
    pub log_format: Option<String>,

    /// 日志文件路径
    #[arg(long, value_name = "PATH", help = "日志输出文件路径")]
    pub log_file: Option<String>,

    /// 数据库连接 URL
    #[arg(long, value_name = "URL", help = "数据库连接字符串")]
    pub database_url: Option<String>,

    /// Redis 连接 URL
    #[arg(long, value_name = "URL", help = "Redis 连接字符串")]
    pub redis_url: Option<String>,

    /// JWT 密钥
    #[arg(long, value_name = "SECRET", help = "JWT 签名密钥")]
    pub jwt_secret: Option<String>,

    /// 启用监控指标
    #[arg(long, help = "启用 Prometheus 监控指标")]
    pub enable_metrics: bool,

    /// 监控端口
    #[arg(long, value_name = "PORT", help = "监控指标服务端口")]
    pub metrics_port: Option<u16>,

    /// 详细输出（可重复使用：-v, -vv, -vvv）
    #[arg(short, action = clap::ArgAction::Count, help = "详细输出级别")]
    pub verbose: u8,

    /// 静默模式
    #[arg(long, short = 'q', help = "静默模式（不输出日志）")]
    pub quiet: bool,

    /// 开发模式（等同于 --env development --log-level debug --log-format pretty）
    #[arg(long, help = "启用开发模式")]
    pub dev: bool,

    /// 子命令
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// 执行数据库迁移（建表/更新表结构）
    Migrate,
    /// 生成默认配置文件
    GenerateConfig {
        /// 输出文件路径
        #[arg(value_name = "PATH", default_value = "config.toml")]
        path: String,
    },
    /// 验证配置文件
    ValidateConfig {
        /// 配置文件路径
        #[arg(value_name = "PATH", default_value = "config.toml")]
        path: String,
    },
    /// 显示最终配置（合并后的配置）
    ShowConfig,
    /// 把只存在于 Redis 里的隐私设置回填进数据库。
    ///
    /// 🔴 上线「DB 为真源」之前**必须先跑**：旧实现只写缓存，线上此刻
    /// 用户改过的隐私设置只存在于 Redis；直接上线会让它们在缓存过期或
    /// 重启后回落成默认值——而默认是允许陌生人发消息。
    ///
    /// 输入是 ops 从 Redis 导出的 JSONL，每行 `{"user_id": 1, "settings": {...}}`：
    ///
    /// ```bash
    /// redis-cli --scan --pattern 'privacy_settings:*' | while read k; do
    ///   printf '{"user_id": %s, "settings": %s}\n' "${k##*:}" "$(redis-cli get "$k")"
    /// done > privacy.jsonl
    /// ```
    BackfillPrivacySettings {
        /// JSONL 文件路径。
        #[arg(long)]
        input: String,
        /// 只统计不写入。
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// 回填消息 → 文件引用表（MEDIA_REFERENCE_AND_FORWARD_SPEC §9）。
    ///
    /// 可断点续跑、可重复执行。**不是 migration**：回填必须走与发送路径同一个
    /// extractor，而 migration runner 只能执行 SQL。
    BackfillMediaRefs {
        /// 一次事务处理多少条消息。
        #[arg(long, default_value_t = 1000)]
        batch_size: i64,
        /// 只回填该毫秒时间戳之后创建的消息（双写上线后的 catch-up 扫描）。
        #[arg(long, default_value_t = 0)]
        since: i64,
        /// 跳过写入，只跑零缺口校验。
        #[arg(long, default_value_t = false)]
        verify_only: bool,
    },
}

impl Cli {
    /// 获取日志级别（考虑 verbose 和 quiet）
    pub fn get_log_level(&self) -> Option<String> {
        if self.quiet {
            return Some("error".to_string());
        }

        if self.dev {
            return Some("debug".to_string());
        }

        if let Some(level) = &self.log_level {
            return Some(level.clone());
        }

        // 根据 verbose 级别设置
        match self.verbose {
            0 => None, // 使用默认或配置文件
            1 => Some("info".to_string()),
            2 => Some("debug".to_string()),
            _ => Some("trace".to_string()),
        }
    }

    /// 获取日志格式
    pub fn get_log_format(&self) -> Option<String> {
        if self.dev {
            return Some("pretty".to_string());
        }
        self.log_format.clone()
    }
}
