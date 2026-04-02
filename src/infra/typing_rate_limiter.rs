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

//! Typing 限频器
//!
//! 按 (user_id, channel_id) 限频，防止高频 typing 事件压垮 pubsub 广播。
//! 规则：同一 (user_id, channel_id) 在 MIN_INTERVAL 内只允许 1 次广播，超出直接丢弃。
//!
//! 清理策略（二合一触发）：
//! - 条件 A（容量触发）：len > CLEANUP_THRESHOLD 时立即清理
//! - 条件 B（时间触发）：len >= CLEANUP_TIME_TRIGGER_THRESHOLD 且距上次清理 >= CLEANUP_MIN_INTERVAL
//! - 清理本身为固定预算 O(CLEANUP_BUDGET)，不引入热路径抖动
//! - CAS 抢占保证同一时刻只有一个线程执行清理

use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 最小广播间隔（500ms）
const MIN_INTERVAL: Duration = Duration::from_millis(500);

/// 过期条目清理阈值：超过此值时容量触发清理（条件 A）
const CLEANUP_THRESHOLD: usize = 10_000;

/// 时间触发清理的最小条目数：低于此值不做周期清理（条件 B）
const CLEANUP_TIME_TRIGGER_THRESHOLD: usize = 1_024;

/// 清理最小间隔：时间触发的节流窗口（条件 B）
const CLEANUP_MIN_INTERVAL: Duration = Duration::from_secs(2);

/// 条目过期时间：超过此时长的条目视为僵尸
const EXPIRE_THRESHOLD: Duration = Duration::from_secs(10);

/// 每次清理扫描的最大条目数（固定预算，避免全量遍历的热路径抖动）
const CLEANUP_BUDGET: usize = 256;

pub struct TypingRateLimiter {
    /// (user_id, channel_id) -> 上次允许广播的时刻
    last_allowed: DashMap<(u64, u64), Instant>,
    /// 进程启动时刻，用于计算 monotonic 毫秒（避免 SystemTime 回拨）
    start: Instant,
    /// 上次清理的 monotonic 毫秒（基于 start）
    last_cleanup_ms: AtomicU64,
    /// 被限频丢弃的总次数
    dropped_count: AtomicU64,
    /// cleanup_expired() 实际执行的次数（CAS 成功后才计数）
    cleanup_runs: AtomicU64,
    /// cleanup_expired() 累计移除的过期条目数
    cleanup_removed: AtomicU64,
}

impl TypingRateLimiter {
    pub fn new() -> Self {
        Self {
            last_allowed: DashMap::new(),
            start: Instant::now(),
            last_cleanup_ms: AtomicU64::new(0),
            dropped_count: AtomicU64::new(0),
            cleanup_runs: AtomicU64::new(0),
            cleanup_removed: AtomicU64::new(0),
        }
    }

    /// 检查是否允许广播。
    /// 返回 true 表示允许，false 表示被限频丢弃。
    pub fn check_and_update(&self, user_id: u64, channel_id: u64) -> bool {
        let key = (user_id, channel_id);
        let now = Instant::now();

        // 快速路径：已有记录，检查间隔
        if let Some(mut entry) = self.last_allowed.get_mut(&key) {
            if now.duration_since(*entry) < MIN_INTERVAL {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            *entry = now;
            drop(entry);
            self.maybe_cleanup();
            return true;
        }

        // 慢路径：首次请求，插入记录
        self.last_allowed.insert(key, now);
        self.maybe_cleanup();

        true
    }

    /// 二合一触发：容量触发 + 时间触发（节流）
    fn maybe_cleanup(&self) {
        let len = self.last_allowed.len();
        if len == 0 {
            return;
        }

        // 条件 A：容量触发 — 超阈值立即清理（仍受 CAS 节流保护）
        if len > CLEANUP_THRESHOLD {
            self.cleanup_expired_with_throttle();
            return;
        }

        // 条件 B：时间触发 — 有一定规模时周期性清理僵尸
        if len >= CLEANUP_TIME_TRIGGER_THRESHOLD {
            self.cleanup_expired_with_throttle();
        }
    }

    /// CAS 节流清理：同一时刻只有一个线程执行，且距上次清理 >= CLEANUP_MIN_INTERVAL
    fn cleanup_expired_with_throttle(&self) {
        let now_ms = self.start.elapsed().as_millis() as u64;
        let last = self.last_cleanup_ms.load(Ordering::Relaxed);

        // 未到最小间隔，跳过
        if now_ms.saturating_sub(last) < CLEANUP_MIN_INTERVAL.as_millis() as u64 {
            return;
        }

        // CAS 抢占清理执行权，失败说明另一线程已在清理
        if self
            .last_cleanup_ms
            .compare_exchange(last, now_ms, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return;
        }

        self.cleanup_runs.fetch_add(1, Ordering::Relaxed);
        self.cleanup_expired();
    }

    /// 固定预算清理：扫描最多 CLEANUP_BUDGET 个条目，移除过期条目。
    /// 多次触发后自然覆盖全表（DashMap iter 顺序跨 shard），避免单次全量遍历的抖动。
    fn cleanup_expired(&self) {
        let now = Instant::now();
        let mut expired_keys = Vec::new();

        for (scanned, entry) in self.last_allowed.iter().enumerate() {
            if scanned >= CLEANUP_BUDGET {
                break;
            }
            if now.duration_since(*entry.value()) >= EXPIRE_THRESHOLD {
                expired_keys.push(entry.key().clone());
            }
        }

        let mut removed = 0u64;
        for key in expired_keys {
            // 二次检查：remove 前再验证是否仍过期（避免竞态误删刚更新的条目）
            if let Some(entry) = self.last_allowed.get(&key) {
                if now.duration_since(*entry.value()) >= EXPIRE_THRESHOLD {
                    drop(entry);
                    self.last_allowed.remove(&key);
                    removed += 1;
                }
            }
        }

        if removed > 0 {
            self.cleanup_removed.fetch_add(removed, Ordering::Relaxed);
        }
    }

    /// 累计被限频丢弃的请求数
    pub fn dropped_total(&self) -> u64 {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// 当前跟踪的 (user, channel) 对数（可对接 gauge 指标）
    pub fn tracked_pairs(&self) -> usize {
        self.last_allowed.len()
    }

    /// cleanup_expired() 累计执行次数（CAS 成功后才计数）
    pub fn cleanup_runs_total(&self) -> u64 {
        self.cleanup_runs.load(Ordering::Relaxed)
    }

    /// cleanup_expired() 累计移除的过期条目数
    pub fn cleanup_removed_total(&self) -> u64 {
        self.cleanup_removed.load(Ordering::Relaxed)
    }
}

impl Default for TypingRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_first_request_allowed() {
        let limiter = TypingRateLimiter::new();
        assert!(limiter.check_and_update(1, 100));
    }

    #[test]
    fn test_rapid_requests_throttled() {
        let limiter = TypingRateLimiter::new();

        // 第一次允许
        assert!(limiter.check_and_update(1, 100));
        // 立即再请求，应被限频
        assert!(!limiter.check_and_update(1, 100));
        assert_eq!(limiter.dropped_total(), 1);
    }

    #[test]
    fn test_different_channels_independent() {
        let limiter = TypingRateLimiter::new();

        assert!(limiter.check_and_update(1, 100));
        // 不同 channel 不受限制
        assert!(limiter.check_and_update(1, 200));
    }

    #[test]
    fn test_different_users_independent() {
        let limiter = TypingRateLimiter::new();

        assert!(limiter.check_and_update(1, 100));
        // 不同 user 不受限制
        assert!(limiter.check_and_update(2, 100));
    }

    #[test]
    fn test_after_interval_allowed() {
        let limiter = TypingRateLimiter::new();

        assert!(limiter.check_and_update(1, 100));
        // 等待超过 MIN_INTERVAL
        thread::sleep(Duration::from_millis(550));
        assert!(limiter.check_and_update(1, 100));
    }

    #[test]
    fn test_cleanup_throttle_prevents_concurrent() {
        let limiter = TypingRateLimiter::new();

        // 插入足够条目触发时间触发阈值
        for i in 0..1100 {
            limiter.last_allowed.insert((i, 0), Instant::now());
        }

        // 首次清理应能获取执行权
        limiter.cleanup_expired_with_throttle();

        // 立即再调用，应被 CAS 节流跳过（间隔 < 2s）
        let before = limiter.tracked_pairs();
        limiter.cleanup_expired_with_throttle();
        // 条目数不应再减少（第二次被节流跳过）
        assert_eq!(limiter.tracked_pairs(), before);
    }
}
