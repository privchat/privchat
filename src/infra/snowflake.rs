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

//! Snowflake ID 生成器
//!
//! 使用 snowflake-me 库生成全局唯一的、时间有序的消息ID
//!
//! 结构：41位时间戳 + 5位数据中心ID + 5位机器ID + 12位序列号
//! - 时间戳：41位（约69年）
//! - 数据中心ID：5位（0-31，32个数据中心）
//! - 机器ID：5位（0-31，每数据中心32台机器）
//! - 序列号：12位（0-4095，每毫秒4096个ID）
//!
//! 单机QPS：4096/ms = 409万/秒

use snowflake_me::Snowflake;
use std::sync::{Mutex, OnceLock};

/// 消息ID生成器（全局单例，线程安全）
///
/// 使用 OnceLock + Mutex 确保线程安全，snowflake-me 内部使用原子操作，无锁设计
///
/// snowflake_me 的默认配置：
/// - 时间戳：41位
/// - 数据中心ID：5位（从环境变量读取，默认1）
/// - 机器ID：5位（从环境变量读取，默认1）
/// - 序列号：12位
static MESSAGE_ID_GENERATOR: OnceLock<Mutex<Snowflake>> = OnceLock::new();

/// 初始化消息ID生成器（线程安全，只初始化一次）
fn init_generator() -> &'static Mutex<Snowflake> {
    MESSAGE_ID_GENERATOR.get_or_init(|| {
        // 从环境变量读取配置
        let data_center_id = std::env::var("SNOWFLAKE_DATA_CENTER_ID")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1u8);

        // P1-12：machine_id 未显式配置时按 pid+启动时间 hash 自动分散（best-effort，
        // 5 位空间无法保证无碰撞），并大声警告——生产多实例部署必须显式配置。
        let machine_id: u8 = match std::env::var("SNOWFLAKE_MACHINE_ID")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            Some(v) => v,
            None => {
                let seed = (std::process::id() as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .wrapping_add(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_nanos() as u64)
                            .unwrap_or(0),
                    );
                let auto = (seed % 32) as u8;
                tracing::warn!(
                    "⚠️ SNOWFLAKE_MACHINE_ID 未配置，自动分配 machine_id={}（pid+time hash，\
                     best-effort 防同机多实例碰撞）。生产多实例部署必须显式配置 \
                     SNOWFLAKE_MACHINE_ID / SNOWFLAKE_DATA_CENTER_ID，否则消息 ID 可能冲突",
                    auto
                );
                auto
            }
        };

        tracing::info!(
            "初始化 Snowflake ID 生成器: data_center_id={}, machine_id={}",
            data_center_id,
            machine_id
        );

        // ⭐ 使用 Builder 模式手动指定 machine_id 和 data_center_id，避免 IP 地址检测失败
        let snowflake = Snowflake::builder()
            .machine_id(&|| Ok(machine_id as u16))
            .data_center_id(&|| Ok(data_center_id as u16))
            .finalize()
            .expect("Failed to initialize Snowflake ID generator");

        Mutex::new(snowflake)
    })
}

/// 生成下一个消息ID
///
/// # 返回
///
/// 返回一个全局唯一的、时间有序的 u64 ID
///
/// # 示例
///
/// ```rust
/// use privchat::infra::snowflake::next_message_id;
///
/// let message_id = next_message_id();
/// println!("Generated message ID: {}", message_id);
/// ```
pub fn next_message_id() -> u64 {
    let generator = init_generator();
    // P1-12：锁污染不 panic——Snowflake 内部状态是原子的，持锁线程 panic 不会
    // 破坏其一致性，恢复继续用。
    let guard = match generator.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!("Snowflake generator lock poisoned，恢复继续使用");
            poisoned.into_inner()
        }
    };
    // P1-12：时钟回拨不 panic——snowflake_me 在回拨时返回 Err，此前 expect 直接
    // 崩掉消息路径。改为等待时钟追平（1ms 步进重试）；回拨期间该路径阻塞（正确：
    // 时钟没追上任何实例都不能安全发号），每累计 1s 打一次 error 供告警。
    let mut attempts: u64 = 0;
    loop {
        match guard.next_id() {
            Ok(id) => return id,
            Err(e) => {
                attempts += 1;
                if attempts % 1000 == 0 {
                    tracing::error!(
                        "Snowflake 时钟回拨持续 ~{}ms 仍未恢复: {}",
                        attempts,
                        e
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
    }
}

/// 生成下一个频道ID
///
/// 频道ID与消息ID使用相同的雪花算法，确保全局唯一性
pub fn next_channel_id() -> u64 {
    next_message_id()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_snowflake_id_generation() {
        let id1 = next_message_id();
        let id2 = next_message_id();

        assert!(id2 > id1, "IDs should be monotonically increasing");
        assert_ne!(id1, id2, "IDs should be unique");
    }

    #[test]
    fn test_snowflake_id_uniqueness() {
        use std::collections::HashSet;
        use std::sync::Arc;
        use std::thread;

        let mut handles = vec![];
        let ids = Arc::new(std::sync::Mutex::new(HashSet::new()));

        // 生成1000个ID，使用10个线程
        for _ in 0..10 {
            let ids_clone = Arc::clone(&ids);
            let handle = thread::spawn(move || {
                let mut thread_ids = HashSet::new();
                for _ in 0..100 {
                    let id = next_message_id();
                    assert!(
                        !thread_ids.contains(&id),
                        "ID should be unique within thread"
                    );
                    thread_ids.insert(id);
                }
                ids_clone.lock().unwrap().extend(thread_ids);
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.join().unwrap();
        }

        // 验证所有ID都是唯一的
        assert_eq!(ids.lock().unwrap().len(), 1000, "All IDs should be unique");
    }
}
