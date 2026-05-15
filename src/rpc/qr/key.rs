// Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
// https://privchat.dev
//
// Author: zoujiaqing <zoujiaqing@gmail.com>
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.

//! QR_CODE_SPEC v1.3 — qr_key 生成器与冲突检测原语。
//!
//! ## 生成器
//! [`generate_qr_key`] 返回 16 位 base62 (`[A-Za-z0-9]`) opaque token，
//! ≈ 95 bits 熵。**禁止**包含 user_id / group_id 或任何可推断信息。
//!
//! ## 冲突重试
//! 理论冲突概率 ≈ 1/2^95，但仍提供 [`QR_KEY_MAX_GENERATION_RETRIES`]
//! 防御性上限，供 INSERT 调用点循环使用。
//!
//! [`is_qr_key_unique_violation`] 判定一个 [`sqlx::Error`] 是否来自
//! `qr_key` UNIQUE 约束（约束名包含 "qr_key"），避免和 username / email
//! 之类的其他 UNIQUE 冲突混淆。

use rand::Rng;

/// qr_key 字符长度，spec §3 锁定 16。
pub const QR_KEY_LEN: usize = 16;

/// 生成器允许的字符表：URL-safe base62（数字 + 大小写字母）。
const QR_KEY_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// INSERT 时遇 qr_key UNIQUE 冲突的最大重试次数。
///
/// 理论概率极低（16-base62 ≈ 95 bits），但保留 5 次重试作为防御性兜底。
pub const QR_KEY_MAX_GENERATION_RETRIES: usize = 5;

/// 生成一个新的 qr_key。**每次调用结果独立、不可预测**。
pub fn generate_qr_key() -> String {
    let mut rng = rand::thread_rng();
    (0..QR_KEY_LEN)
        .map(|_| {
            let idx = rng.gen_range(0..QR_KEY_ALPHABET.len());
            QR_KEY_ALPHABET[idx] as char
        })
        .collect()
}

/// 判定 `err` 是否来自 `qr_key` UNIQUE 约束冲突。
///
/// PostgreSQL `unique_violation` 错误码 = `23505`，约束名由 migration 018
/// 定为 `ux_privchat_users_qr_key` / `ux_privchat_groups_qr_key`，两者都
/// 包含 `qr_key` 子串。其他 UNIQUE 冲突（username / email / phone 等）
/// 由调用方走原有的 `DuplicateEntry` 错误路径，不被本函数误判。
pub fn is_qr_key_unique_violation(err: &sqlx::Error) -> bool {
    if let sqlx::Error::Database(db_err) = err {
        if db_err.code().as_deref() == Some("23505") {
            if let Some(constraint) = db_err.constraint() {
                return constraint.contains("qr_key");
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn generate_qr_key_is_16_chars() {
        let k = generate_qr_key();
        assert_eq!(k.len(), QR_KEY_LEN);
        assert_eq!(k.chars().count(), QR_KEY_LEN);
    }

    #[test]
    fn generate_qr_key_uses_base62_alphabet_only() {
        for _ in 0..1024 {
            let k = generate_qr_key();
            for c in k.chars() {
                assert!(
                    c.is_ascii_alphanumeric(),
                    "char {:?} not in base62 alphabet",
                    c
                );
            }
        }
    }

    #[test]
    fn generate_qr_key_is_unlikely_to_collide() {
        // 1000 次生成，期望全部唯一（理论碰撞 ≈ 1e-23）
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(generate_qr_key()), "unexpected collision");
        }
    }

    #[test]
    fn generate_qr_key_does_not_embed_predictable_pattern() {
        // 两次生成不能相等；多次生成不应该有可见前缀模式
        let a = generate_qr_key();
        let b = generate_qr_key();
        assert_ne!(a, b);
        // 前 4 位的取值多样性（健康度量；不严格断言但保证不是常量）
        let prefixes: HashSet<_> = (0..64).map(|_| generate_qr_key()[..4].to_string()).collect();
        assert!(prefixes.len() > 32, "qr_key prefix entropy too low");
    }
}
