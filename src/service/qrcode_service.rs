use chrono::{Duration, Utc};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::error::ServerError;
use crate::model::{QRKeyOptions, QRKeyRecord, QRScanLog, QRType};

/// QR 码服务
///
/// 负责管理二维码的生成、解析、刷新、撤销等功能
///
/// 支持的二维码类型：
/// - 用户名片（User）
/// - 群组邀请（Group）
/// - 扫码登录（Auth）
/// - 其他功能（Feature）
///
/// 性能优化：使用 DashMap 替代 Arc<RwLock<HashMap>>，减少锁竞争
#[derive(Clone)]
pub struct QRCodeService {
    /// QR Key 存储：qr_key -> QRKeyRecord
    /// TODO: 后续改为 Redis 或 PostgreSQL
    storage: DashMap<String, QRKeyRecord>,

    /// 反向索引：target_id -> qr_key（用于快速查找和撤销）
    target_index: DashMap<String, String>,

    /// 扫描日志存储（Vec 需要 RwLock，因为需要追加操作）
    scan_logs: Arc<RwLock<Vec<QRScanLog>>>,
}

impl QRCodeService {
    /// 创建新的 QR 码服务
    pub fn new() -> Self {
        Self {
            storage: DashMap::new(),
            target_index: DashMap::new(),
            scan_logs: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// 生成新的 QR Key
    ///
    /// # 参数
    /// - qr_type: QR 码类型
    /// - target_id: 目标 ID（user_id 或 group_id）
    /// - creator_id: 创建者 ID
    /// - options: 生成选项
    ///
    /// # 返回
    /// 生成的 QRKeyRecord
    pub async fn generate(
        &self,
        qr_type: QRType,
        target_id: String,
        creator_id: String,
        options: QRKeyOptions,
    ) -> Result<QRKeyRecord, ServerError> {
        // 1. 如果需要，撤销该目标的旧 QR Key
        if options.revoke_old {
            let _ = self.revoke_by_target(&target_id).await;
        }

        // 2. 生成随机 QR Key（12 位字母数字）
        let qr_key = Self::generate_random_key();

        // 3. 计算过期时间
        let expire_at = options
            .expire_seconds
            .map(|s| Utc::now() + Duration::seconds(s));

        // 4. 创建记录
        let record = QRKeyRecord {
            qr_key: qr_key.clone(),
            qr_type,
            target_id: target_id.clone(),
            creator_id,
            created_at: Utc::now(),
            expire_at,
            max_usage: if options.one_time {
                Some(1)
            } else {
                options.max_usage
            },
            used_count: 0,
            revoked: false,
            metadata: options.metadata,
        };

        // 5. 保存记录（DashMap 无需显式锁）
        self.storage.insert(qr_key.clone(), record.clone());

        // 6. 更新反向索引
        self.target_index.insert(target_id.clone(), qr_key.clone());

        tracing::info!(
            "✅ QR Key 生成成功: type={}, target={}, qr_key={}",
            record.qr_type.as_str(),
            target_id,
            qr_key
        );

        Ok(record)
    }

    /// 解析 QR Key
    ///
    /// # 参数
    /// - qr_key: 要解析的 QR Key
    /// - scanner_id: 扫描者 ID
    /// - token: 可选的额外验证 token（群组邀请时需要）
    ///
    /// # 返回
    /// QRKeyRecord（包含目标信息）
    pub async fn resolve(
        &self,
        qr_key: &str,
        scanner_id: u64,
        token: Option<&str>,
    ) -> Result<QRKeyRecord, ServerError> {
        // 1. 查找记录（DashMap 使用 get_mut 获取可变引用）
        let mut record = self
            .storage
            .get_mut(qr_key)
            .ok_or_else(|| ServerError::NotFound("QR Key 不存在或已失效".to_string()))?;

        // 2. 验证状态
        if record.revoked {
            self.log_scan_failure(qr_key, &scanner_id.to_string(), "QR Key 已被撤销")
                .await;
            return Err(ServerError::BadRequest("QR Key 已被撤销".to_string()));
        }

        // 3. 验证过期时间
        if record.is_expired() {
            self.log_scan_failure(qr_key, &scanner_id.to_string(), "QR Key 已过期")
                .await;
            return Err(ServerError::BadRequest("QR Key 已过期".to_string()));
        }

        // 4. 验证使用次数
        if record.is_usage_exceeded() {
            self.log_scan_failure(qr_key, &scanner_id.to_string(), "QR Key 使用次数已达上限")
                .await;
            return Err(ServerError::BadRequest(
                "QR Key 使用次数已达上限".to_string(),
            ));
        }

        // 5. 验证额外的 token（群组邀请时需要）
        if record.qr_type == QRType::Group {
            if let Some(expected_token) = record.metadata.get("token").and_then(|t| t.as_str()) {
                if token != Some(expected_token) {
                    self.log_scan_failure(qr_key, &scanner_id.to_string(), "Token 验证失败")
                        .await;
                    return Err(ServerError::Unauthorized("Token 验证失败".to_string()));
                }
            }
        }

        // 6. 更新使用次数
        record.used_count += 1;

        // 7. 记录扫描日志（审计）
        self.log_scan_success(qr_key, &scanner_id.to_string()).await;

        tracing::info!(
            "✅ QR Key 解析成功: qr_key={}, scanner={}, used_count={}",
            qr_key,
            scanner_id,
            record.used_count
        );

        Ok(record.clone())
    }

    /// 刷新 QR Key（撤销旧的，生成新的）
    ///
    /// # 参数
    /// - target_id: 目标 ID
    /// - qr_type: QR 码类型
    /// - creator_id: 创建者 ID
    ///
    /// # 返回
    /// (old_qr_key, new_qr_key)
    pub async fn refresh(
        &self,
        target_id: &str,
        qr_type: QRType,
        creator_id: &str,
    ) -> Result<(String, String), ServerError> {
        // 1. 撤销旧的 QR Key
        let old_qr_key = self
            .revoke_by_target(target_id)
            .await
            .unwrap_or_else(|_| String::new());

        // 2. 生成新的 QR Key
        let new_record = self
            .generate(
                qr_type,
                target_id.to_string(),
                creator_id.to_string(),
                QRKeyOptions {
                    revoke_old: false, // 已经撤销了，不需要再次撤销
                    ..Default::default()
                },
            )
            .await?;

        tracing::info!(
            "✅ QR Key 刷新成功: target={}, old={}, new={}",
            target_id,
            old_qr_key,
            new_record.qr_key
        );

        Ok((old_qr_key, new_record.qr_key))
    }

    /// 撤销指定的 QR Key
    pub async fn revoke(&self, qr_key: &str) -> Result<(), ServerError> {
        let mut record = self
            .storage
            .get_mut(qr_key)
            .ok_or_else(|| ServerError::NotFound("QR Key 不存在".to_string()))?;

        record.revoked = true;

        tracing::info!("✅ QR Key 已撤销: qr_key={}", qr_key);

        Ok(())
    }

    /// 撤销指定目标的所有 QR Key
    pub async fn revoke_by_target(&self, target_id: &str) -> Result<String, ServerError> {
        // 从反向索引中查找
        if let Some(qr_key_entry) = self.target_index.get(target_id) {
            let qr_key = qr_key_entry.value().clone();
            if let Some(mut record) = self.storage.get_mut(&qr_key) {
                record.revoked = true;
                self.target_index.remove(target_id);

                tracing::info!(
                    "✅ 撤销目标的 QR Key: target={}, qr_key={}",
                    target_id,
                    qr_key
                );

                return Ok(qr_key);
            }
        }

        Err(ServerError::NotFound("未找到该目标的 QR Key".to_string()))
    }

    /// 列出指定用户创建的所有 QR Keys
    pub async fn list_by_creator(
        &self,
        creator_id: &str,
        include_revoked: bool,
    ) -> Vec<QRKeyRecord> {
        self.storage
            .iter()
            .filter(|entry| {
                let record = entry.value();
                record.creator_id == creator_id && (include_revoked || !record.revoked)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// 获取指定 QR Key 的详细信息
    pub async fn get(&self, qr_key: &str) -> Option<QRKeyRecord> {
        self.storage.get(qr_key).map(|entry| entry.value().clone())
    }

    /// 获取指定目标的当前 QR Key
    pub async fn get_by_target(&self, target_id: &str) -> Option<QRKeyRecord> {
        if let Some(qr_key_entry) = self.target_index.get(target_id) {
            let qr_key = qr_key_entry.value();
            return self.storage.get(qr_key).map(|entry| entry.value().clone());
        }
        None
    }

    /// 获取扫描日志
    pub async fn get_scan_logs(&self, qr_key: &str) -> Vec<QRScanLog> {
        let scan_logs = self.scan_logs.read().await;
        scan_logs
            .iter()
            .filter(|log| log.qr_key == qr_key)
            .cloned()
            .collect()
    }

    /// 生成随机 QR Key（12位字母数字）
    fn generate_random_key() -> String {
        use rand::Rng;
        const CHARSET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let mut rng = rand::thread_rng();

        (0..12)
            .map(|_| {
                let idx = rng.gen_range(0..CHARSET.len());
                CHARSET[idx] as char
            })
            .collect()
    }

    /// 记录扫描成功日志
    async fn log_scan_success(&self, qr_key: &str, scanner_id: &str) {
        let log = QRScanLog {
            qr_key: qr_key.to_string(),
            scanner_id: scanner_id.to_string(),
            scanned_at: Utc::now(),
            success: true,
            error: None,
        };

        let mut scan_logs = self.scan_logs.write().await;
        scan_logs.push(log);

        tracing::info!(
            "📱 QR Code Scanned: qr_key={}, scanner={}",
            qr_key,
            scanner_id
        );
    }

    /// 记录扫描失败日志
    async fn log_scan_failure(&self, qr_key: &str, scanner_id: &str, error: &str) {
        let log = QRScanLog {
            qr_key: qr_key.to_string(),
            scanner_id: scanner_id.to_string(),
            scanned_at: Utc::now(),
            success: false,
            error: Some(error.to_string()),
        };

        let mut scan_logs = self.scan_logs.write().await;
        scan_logs.push(log);

        tracing::warn!(
            "⚠️ QR Code Scan Failed: qr_key={}, scanner={}, error={}",
            qr_key,
            scanner_id,
            error
        );
    }
}

impl Default for QRCodeService {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn test_generate_and_resolve_user_qrcode() {
        let service = QRCodeService::new();

        // 生成用户二维码
        let record = service
            .generate(
                QRType::User,
                "alice".to_string(),
                "alice".to_string(),
                QRKeyOptions::default(),
            )
            .await
            .unwrap();

        assert_eq!(record.qr_type, QRType::User);
        assert_eq!(record.target_id, "alice");
        assert_eq!(record.qr_key.len(), 12);

        // 解析二维码
        let resolved = service.resolve(&record.qr_key, "bob", None).await.unwrap();
        assert_eq!(resolved.target_id, "alice");
        assert_eq!(resolved.used_count, 1);
    }

    #[tokio::test]
    async fn test_generate_and_resolve_group_qrcode() {
        let service = QRCodeService::new();

        // 生成群组二维码（带 token）
        let record = service
            .generate(
                QRType::Group,
                "group_123".to_string(),
                "alice".to_string(),
                QRKeyOptions {
                    metadata: json!({ "token": "abc123" }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 使用正确的 token 解析
        let resolved = service
            .resolve(&record.qr_key, "bob", Some("abc123"))
            .await
            .unwrap();
        assert_eq!(resolved.target_id, "group_123");

        // 使用错误的 token 应该失败
        let result = service
            .resolve(&record.qr_key, "charlie", Some("wrong"))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_refresh_qrcode() {
        let service = QRCodeService::new();

        // 生成初始二维码
        let record1 = service
            .generate(
                QRType::User,
                "alice".to_string(),
                "alice".to_string(),
                QRKeyOptions::default(),
            )
            .await
            .unwrap();

        let old_qr_key = record1.qr_key.clone();

        // 刷新二维码
        let (revoked_key, new_qr_key) = service
            .refresh("alice", QRType::User, "alice")
            .await
            .unwrap();

        assert_eq!(revoked_key, old_qr_key);
        assert_ne!(new_qr_key, old_qr_key);

        // 旧的应该被撤销
        let old_record = service.get(&old_qr_key).await.unwrap();
        assert!(old_record.revoked);

        // 新的应该有效
        let new_record = service.get(&new_qr_key).await.unwrap();
        assert!(!new_record.revoked);
    }

    #[tokio::test]
    async fn test_one_time_qrcode() {
        let service = QRCodeService::new();

        // 生成一次性二维码
        let record = service
            .generate(
                QRType::Auth,
                "login_session_123".to_string(),
                "alice".to_string(),
                QRKeyOptions {
                    one_time: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // 第一次解析成功
        let _ = service
            .resolve(&record.qr_key, "alice", None)
            .await
            .unwrap();

        // 第二次解析应该失败（已达使用上限）
        let result = service.resolve(&record.qr_key, "alice", None).await;
        assert!(result.is_err());
    }
}
