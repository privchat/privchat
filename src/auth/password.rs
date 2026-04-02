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

use crate::error::DatabaseError;
/// 密码加密和验证模块
///
/// 使用 bcrypt 算法进行密码加密（行业标准）
use bcrypt::{hash, verify, DEFAULT_COST};

/// 密码加密成本（默认值12，适合大多数场景）
///
/// 成本值越高，加密越安全，但也越慢：
/// - 10: 约 80ms（适合高并发场景）
/// - 12: 约 300ms（默认，平衡安全和性能）
/// - 14: 约 1200ms（高安全场景）
pub const PASSWORD_COST: u32 = DEFAULT_COST; // 12

/// 加密密码
///
/// 使用 bcrypt 算法将明文密码加密为哈希值
///
/// # 参数
/// - password: 明文密码
///
/// # 返回
/// - Ok(String): 加密后的密码哈希（60字符）
/// - Err: 加密失败
///
/// # 示例
/// ```
/// use privchat::auth::hash_password;
/// use privchat::error::DatabaseError;
///
/// fn main() -> Result<(), DatabaseError> {
/// let hash = hash_password("secret123")?;
/// // 输出类似: $2b$12$LQv3c1yqBWVHxkd0LHAkCOYz6TtxMQJqhN8/LewY5GyYKwqHhKbCS
/// # Ok(())
/// # }
/// ```
pub fn hash_password(password: &str) -> Result<String, DatabaseError> {
    hash(password, PASSWORD_COST)
        .map_err(|e| DatabaseError::Internal(format!("密码加密失败: {}", e)))
}

/// 验证密码
///
/// 比较明文密码和存储的哈希值是否匹配
///
/// # 参数
/// - password: 明文密码
/// - hash: 存储的密码哈希
///
/// # 返回
/// - Ok(true): 密码匹配
/// - Ok(false): 密码不匹配
/// - Err: 验证过程出错
///
/// # 示例
/// ```
/// use privchat::auth::verify_password;
/// use privchat::error::DatabaseError;
///
/// fn main() -> Result<(), DatabaseError> {
/// let hash = "$2b$12$LQv3c1yqBWVHxkd0LHAkCOYz6TtxMQJqhN8/LewY5GyYKwqHhKbCS";
/// let valid = verify_password("secret123", hash)?;  // true
/// let invalid = verify_password("wrong", hash)?;    // false
/// # let _ = (valid, invalid);
/// # Ok(())
/// # }
/// ```
pub fn verify_password(password: &str, hash: &str) -> Result<bool, DatabaseError> {
    verify(password, hash).map_err(|e| DatabaseError::Internal(format!("密码验证失败: {}", e)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_password() {
        let password = "secret123";
        let hash = hash_password(password).unwrap();

        // bcrypt 哈希总是 60 字符
        assert_eq!(hash.len(), 60);
        // bcrypt 哈希以 $2b$ 开头
        assert!(hash.starts_with("$2b$"));
    }

    #[test]
    fn test_verify_password_correct() {
        let password = "secret123";
        let hash = hash_password(password).unwrap();

        // 正确的密码应该验证成功
        assert!(verify_password(password, &hash).unwrap());
    }

    #[test]
    fn test_verify_password_wrong() {
        let password = "secret123";
        let hash = hash_password(password).unwrap();

        // 错误的密码应该验证失败
        assert!(!verify_password("wrong_password", &hash).unwrap());
    }

    #[test]
    fn test_same_password_different_hash() {
        let password = "secret123";
        let hash1 = hash_password(password).unwrap();
        let hash2 = hash_password(password).unwrap();

        // 相同密码的哈希值应该不同（因为 salt 不同）
        assert_ne!(hash1, hash2);

        // 但都应该能验证成功
        assert!(verify_password(password, &hash1).unwrap());
        assert!(verify_password(password, &hash2).unwrap());
    }
}
