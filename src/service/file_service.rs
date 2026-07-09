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

//! 文件服务 - 处理文件上传、存储和管理
//!
//! 基于 [OpenDAL](https://opendal.apache.org/) 统一对象存储抽象：本地 FS 与 S3/OSS/COS/MinIO/Garage 等
//! 共用同一套 Operator API（write/read/delete），实现轻量、通用。
//!
//! 上传服务只负责存储，不做压缩/缩略图；类型、大小、业务等以请求上传 token 时的约定为准。

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use opendal::Operator;

use crate::config::FileStorageSourceConfig;
use crate::error::{Result, ServerError};
use crate::repository::FileUploadRepository;

// 向后兼容：从 service 层继续导出类型（upload_token_service 等使用）
pub use crate::model::file_upload::{FileMetadata, FileType};

/// 存储源 ID：0=本地，1=S3 等
pub const STORAGE_SOURCE_LOCAL: u32 = 0;

/// 文件 URL 响应（用于 get_file_url）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUrlResponse {
    pub file_url: String,
    pub thumbnail_url: Option<String>,
    pub expires_at: i64,
    pub file_size: u64,
    pub mime_type: String,
    pub storage_source_id: u32,
    /// 附件加密版本：0=明文；1=AES-256-GCM。
    pub encryption_version: i32,
    /// CEK（base64url 32B）；仅鉴权后返回，绝不进日志/URL。version=0 时 None。
    pub cek: Option<String>,
}

/// 附件访问授权纯决策（无 IO，便于单测；ATTACHMENT_ENCRYPTION_SPEC §授权）。
///
/// 由调用方先把 IO 解析好再传入：
/// - `bound_message_id`：file 绑定的 message_id；`None` = 未绑定（pending）。
/// - `member_of_message_channel`：当 bound 时，当前用户是否是该 message 所在 channel 成员；
///   `Some(true)`=是成员；`Some(false)`=非成员；`None`=消息/channel 无法解析（broken binding）。
///
/// 规则：pending → 仅 uploader；bound → 必须是成员（broken/非成员一律拒绝，不 fallback 放行）。
/// 注意：bound 文件即使是 uploader，若非成员也拒绝（不能靠 uploader 身份绕过）。
pub fn authorize_file_access(
    user_id: u64,
    uploader_id: u64,
    bound_message_id: Option<u64>,
    member_of_message_channel: Option<bool>,
) -> bool {
    match bound_message_id {
        None => uploader_id == user_id,
        Some(_) => member_of_message_channel == Some(true),
    }
}

/// 文件服务（多存储源，按 default_storage_source_id 选择；存储层统一用 OpenDAL Operator）
pub struct FileService {
    sources_by_id: HashMap<u32, FileStorageSourceConfig>,
    /// 每个存储源对应一个 OpenDAL Operator（在 init 中按配置构建）
    operators: Arc<RwLock<HashMap<u32, Operator>>>,
    default_storage_source_id: u32,
    file_upload_repo: Arc<FileUploadRepository>,
}

impl FileService {
    pub fn new(
        sources: Vec<FileStorageSourceConfig>,
        default_storage_source_id: u32,
        pool: Arc<sqlx::PgPool>,
    ) -> Self {
        let sources_by_id = sources.into_iter().map(|s| (s.id, s)).collect();
        Self {
            sources_by_id,
            operators: Arc::new(RwLock::new(HashMap::new())),
            default_storage_source_id,
            file_upload_repo: Arc::new(FileUploadRepository::new(pool)),
        }
    }

    pub fn source_count(&self) -> usize {
        self.sources_by_id.len()
    }

    fn resolve_storage_source(&self) -> Result<&FileStorageSourceConfig> {
        self.sources_by_id
            .get(&self.default_storage_source_id)
            .ok_or_else(|| {
                ServerError::Internal(format!(
                    "未找到存储源 id={}，请确保 [file] 中至少配置一个 [[file.storage_sources]] 且 default_storage_source_id 存在",
                    self.default_storage_source_id
                ))
            })
    }

    /// 初始化：为每个存储源构建 OpenDAL Operator（Fs 或 S3），本地 FS 预创建子目录
    pub async fn init(&self) -> Result<()> {
        let subdirs = [
            "images/",
            "videos/",
            "audios/",
            "files/",
            "others/",
            "thumbnails/",
        ];
        for src in self.sources_by_id.values() {
            let op = Self::build_operator(src).await?;
            if src.storage_type == "local" {
                for d in &subdirs {
                    op.create_dir(*d).await.map_err(|e| {
                        ServerError::Internal(format!(
                            "创建存储子目录 \"{}\"（存储源 id={}）失败: {}",
                            d.trim_end_matches('/'),
                            src.id,
                            e
                        ))
                    })?;
                }
            }
            self.operators.write().await.insert(src.id, op);
        }
        Ok(())
    }

    /// 根据配置构建 OpenDAL Operator（兼容标准 Fs / S3 配置）
    async fn build_operator(src: &FileStorageSourceConfig) -> Result<Operator> {
        if src.storage_type == "local" {
            let root = src.storage_root.trim();
            if root.is_empty() {
                return Err(ServerError::Internal(
                    "local 存储源缺少 storage_root".to_string(),
                ));
            }
            let root_path = std::path::Path::new(root);
            // 目录不存在时自动创建，创建失败则返回明确错误
            if !root_path.exists() {
                tokio::fs::create_dir_all(root_path).await.map_err(|e| {
                    ServerError::Internal(format!("创建文件存储目录失败 \"{}\": {}", root, e))
                })?;
            }
            let abs_root = if root_path.is_absolute() {
                root.to_string()
            } else {
                tokio::fs::canonicalize(root_path)
                    .await
                    .map_err(|e| {
                        ServerError::Internal(format!("无法解析 storage_root \"{}\": {}", root, e))
                    })?
                    .to_string_lossy()
                    .to_string()
            };
            let builder = opendal::services::Fs::default().root(&abs_root);
            let op: Operator = Operator::new(builder)
                .map_err(|e| ServerError::Internal(format!("构建 Fs Operator 失败: {}", e)))?
                .finish();
            return Ok(op);
        }
        if src.storage_type == "s3" {
            let endpoint = src
                .endpoint
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 endpoint".to_string()))?
                .trim();
            let bucket = src
                .bucket
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 bucket".to_string()))?
                .trim();
            let access_key_id = src
                .access_key_id
                .as_deref()
                .ok_or_else(|| ServerError::Internal("S3 存储源缺少 access_key_id".to_string()))?
                .trim();
            let secret_access_key = src
                .secret_access_key
                .as_deref()
                .ok_or_else(|| {
                    ServerError::Internal("S3 存储源缺少 secret_access_key".to_string())
                })?
                .trim();
            if endpoint.is_empty()
                || bucket.is_empty()
                || access_key_id.is_empty()
                || secret_access_key.is_empty()
            {
                return Err(ServerError::Internal(
                    "S3 存储源 endpoint / bucket / access_key_id / secret_access_key 均不能为空"
                        .to_string(),
                ));
            }
            let endpoint_url =
                if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
                    endpoint.to_string()
                } else {
                    format!("https://{}", endpoint)
                };
            let mut builder = opendal::services::S3::default()
                .bucket(bucket)
                .endpoint(&endpoint_url)
                .region("auto")
                .access_key_id(access_key_id)
                .secret_access_key(secret_access_key);
            if let Some(ref prefix) = src.path_prefix {
                let p = prefix.trim().trim_end_matches('/');
                if !p.is_empty() {
                    builder = builder.root(p);
                }
            }
            let op: Operator = Operator::new(builder)
                .map_err(|e| ServerError::Internal(format!("构建 S3 Operator 失败: {}", e)))?
                .finish();
            return Ok(op);
        }
        Err(ServerError::Unsupported(format!(
            "不支持的存储类型: {}",
            src.storage_type
        )))
    }

    /// P0-10 流式上传起点：确定类型/存储源/file_id/路径，打开流式 writer。
    /// 数据不再一次性进内存——调用方循环 `write_chunk` 边收边写，超限即时中止。
    /// `token_max_size` 与按类型的服务端上限取 min 作为硬顶。
    pub async fn begin_streaming_upload(
        &self,
        mime_type: &str,
        filename: &str,
        token_max_size: i64,
    ) -> Result<StreamingUpload> {
        let file_type = self.detect_file_type(mime_type)?;
        let type_limit = Self::max_size_for_type(&file_type) as u64;
        let limit = type_limit.min(token_max_size.max(0) as u64);

        let source = self.resolve_storage_source()?;
        let source_id = source.id;
        let op = self
            .operators
            .read()
            .await
            .get(&source_id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={} 的 Operator", source_id))
            })?;

        let file_id = self.file_upload_repo.next_file_id().await?;
        let file_path = self.generate_file_path(file_id, &file_type, filename);
        let writer = op
            .writer(&file_path)
            .await
            .map_err(|e| ServerError::Internal(format!("打开存储 writer 失败: {}", e)))?;

        Ok(StreamingUpload {
            file_id,
            file_path,
            source_id,
            file_type,
            op,
            writer: Some(writer),
            hasher: std::collections::hash_map::DefaultHasher::new(),
            written: 0,
            limit,
        })
    }

    /// P0-10 流式上传收尾：关闭 writer、定稿 hash、落库返回元数据。
    #[allow(clippy::too_many_arguments)]
    pub async fn commit_streaming_upload(
        &self,
        mut upload: StreamingUpload,
        filename: String,
        mime_type: String,
        uploader_id: u64,
        uploader_ip: Option<String>,
        business_type: String,
        business_id: Option<String>,
        encryption_version: i32,
        cek: Option<String>,
    ) -> Result<FileMetadata> {
        use std::hash::Hasher as _;

        let mut writer = upload
            .writer
            .take()
            .ok_or_else(|| ServerError::Internal("上传流已关闭".to_string()))?;
        writer
            .close()
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入收尾失败: {}", e)))?;

        let metadata = FileMetadata {
            file_id: upload.file_id,
            original_filename: filename,
            file_size: upload.written,
            original_size: None,
            file_type: upload.file_type.clone(),
            mime_type,
            file_path: upload.file_path.clone(),
            storage_source_id: upload.source_id,
            uploader_id,
            uploader_ip,
            uploaded_at: chrono::Utc::now().timestamp_millis() as u64,
            width: None,
            height: None,
            file_hash: Some(format!("hash:{}", upload.hasher.finish())),
            business_type: Some(business_type),
            business_id,
            encryption_version,
            cek,
        };
        self.file_upload_repo.insert(&metadata).await?;
        Ok(metadata)
    }

    pub async fn get_file_metadata(&self, file_id: u64) -> Result<Option<FileMetadata>> {
        self.file_upload_repo.get_by_file_id(file_id).await
    }

    pub async fn update_business(
        &self,
        file_id: u64,
        business_type: &str,
        business_id: &str,
    ) -> Result<bool> {
        self.file_upload_repo
            .update_business(file_id, business_type, business_id)
            .await
    }

    pub async fn list_file_ids_by_business(
        &self,
        business_type: &str,
        business_id: &str,
    ) -> Result<Vec<u64>> {
        self.file_upload_repo
            .list_file_ids_by_business(business_type, business_id)
            .await
    }

    pub async fn verify_file_ownership(&self, file_id: u64, user_id: u64) -> Result<bool> {
        Ok(self
            .file_upload_repo
            .get_by_file_id(file_id)
            .await?
            .map(|m| m.uploader_id == user_id)
            .unwrap_or(false))
    }

    pub async fn delete_file(&self, file_id: u64, user_id: u64) -> Result<()> {
        if !self.verify_file_ownership(file_id, user_id).await? {
            return Err(ServerError::Forbidden("无权删除此文件".to_string()));
        }

        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;

        let op = self
            .operators
            .read()
            .await
            .get(&metadata.storage_source_id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={}", metadata.storage_source_id))
            })?;

        let _ = op.delete(&metadata.file_path).await;

        self.file_upload_repo.delete(file_id).await?;
        Ok(())
    }

    fn detect_file_type(&self, mime_type: &str) -> Result<FileType> {
        // 注：这里只按 MIME 服务端兜底分类。Voice 消息的分类由 SDK 明确传入 "voice"，
        // 不靠 MIME 推导——否则任何 audio/* 的普通文件会被误分到 Voice。
        if mime_type.starts_with("image/") {
            Ok(FileType::Image)
        } else if mime_type.starts_with("video/") {
            Ok(FileType::Video)
        } else {
            Ok(FileType::File)
        }
    }

    /// 按文件类型的服务端大小硬顶（流式路径在 write_chunk 中即时校验）。
    fn max_size_for_type(file_type: &FileType) -> usize {
        match file_type {
            FileType::Image => 10 * 1024 * 1024,
            FileType::Video => 100 * 1024 * 1024,
            FileType::Voice => 10 * 1024 * 1024,
            FileType::File => 50 * 1024 * 1024,
            FileType::Other => 10 * 1024 * 1024,
        }
    }

    fn generate_file_path(&self, file_id: u64, file_type: &FileType, filename: &str) -> String {
        let extension = filename.split('.').last().unwrap_or("bin");
        let subdir = match file_type {
            FileType::Image => "images",
            FileType::Video => "videos",
            FileType::Voice => "voices",
            FileType::File => "files",
            FileType::Other => "others",
        };
        format!("{}/{}.{}", subdir, file_id, extension)
    }

    pub async fn get_file_url(&self, file_id: u64, _user_id: u64) -> Result<FileUrlResponse> {
        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;
        let file_url = self.build_access_url(&metadata.file_path, metadata.storage_source_id);
        let expires_at = Utc::now().timestamp() + 3600 * 24 * 365;
        Ok(FileUrlResponse {
            file_url,
            thumbnail_url: None,
            expires_at,
            file_size: metadata.file_size,
            mime_type: metadata.mime_type,
            storage_source_id: metadata.storage_source_id,
            // ⚠️ P0 安全：此处随 detail 返回 CEK，但调用方 get_file_url 目前 **未做** 访问授权
            // （user_id 未使用）。返回 cek 前必须校验当前用户有权访问该附件（file→message→channel
            // 成员）。授权补齐前，加密形同虚设。见 ATTACHMENT_ENCRYPTION_SPEC §授权。
            encryption_version: metadata.encryption_version,
            cek: metadata.cek,
        })
    }

    /// 读取文件内容（用于下载；统一走 OpenDAL read）
    pub async fn read_file(&self, file_id: u64) -> Result<Vec<u8>> {
        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;
        let op = self
            .operators
            .read()
            .await
            .get(&metadata.storage_source_id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={}", metadata.storage_source_id))
            })?;

        let buf = op
            .read(&metadata.file_path)
            .await
            .map_err(|e| ServerError::Internal(format!("存储读取失败: {}", e)))?;
        Ok(buf.to_vec())
    }

    pub fn build_access_url(&self, file_path: &str, storage_source_id: u32) -> String {
        if let Some(src) = self.sources_by_id.get(&storage_source_id) {
            if let Some(base_url) = &src.base_url {
                let base = base_url.trim_end_matches('/');
                // base_url 已包含完整路径，直接拼接 file_path
                return format!("{}/{}", base, file_path);
            }
            return format!("/{}", file_path);
        }
        format!("{{unsupported:storage_source_id={}}}", storage_source_id)
    }
}

/// P0-10 流式上传句柄：由 `begin_streaming_upload` 创建，调用方按 chunk 喂数据，
/// 全程只驻留单个 chunk 的内存。成功走 `commit_streaming_upload` 落库；
/// 失败/校验不过必须调 `abort()` 清掉已写入的半文件。
pub struct StreamingUpload {
    pub file_id: u64,
    pub file_path: String,
    pub source_id: u32,
    file_type: FileType,
    op: Operator,
    writer: Option<opendal::Writer>,
    hasher: std::collections::hash_map::DefaultHasher,
    written: u64,
    limit: u64,
}

impl StreamingUpload {
    /// 已写入字节数（加密结构等收尾校验用）。
    pub fn written(&self) -> u64 {
        self.written
    }

    /// 写入一个 chunk：先做累计大小硬顶校验（超限即时失败，不再继续收 body），
    /// 同步推进增量 hash。
    pub async fn write_chunk(&mut self, chunk: bytes::Bytes) -> Result<()> {
        use std::hash::Hasher as _;

        self.written = self.written.saturating_add(chunk.len() as u64);
        if self.written > self.limit {
            return Err(ServerError::Validation(format!(
                "文件大小超过限制: {} > {} bytes",
                self.written, self.limit
            )));
        }
        self.hasher.write(&chunk);
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| ServerError::Internal("上传流已关闭".to_string()))?;
        writer
            .write(chunk)
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入失败: {}", e)))
    }

    /// 中止上传：尽力关闭 writer 并删除已写入的半文件（不落库）。
    pub async fn abort(mut self) {
        if let Some(mut writer) = self.writer.take() {
            let _ = writer.close().await;
        }
        if let Err(e) = self.op.delete(&self.file_path).await {
            tracing::warn!(
                "⚠️ 清理中止上传的半文件失败 path={}: {}",
                self.file_path,
                e
            );
        }
    }
}

#[cfg(test)]
mod authz_tests {
    use super::authorize_file_access;

    // pending（未绑定 message）：仅 uploader 可访问
    #[test]
    fn pending_uploader_allowed() {
        assert!(authorize_file_access(1, 1, None, None));
    }

    #[test]
    fn pending_non_uploader_denied() {
        assert!(!authorize_file_access(2, 1, None, None));
    }

    // bound（已绑定 message）：成员可访问
    #[test]
    fn bound_member_allowed() {
        assert!(authorize_file_access(2, 1, Some(99), Some(true)));
    }

    #[test]
    fn bound_non_member_denied() {
        assert!(!authorize_file_access(2, 1, Some(99), Some(false)));
    }

    // broken binding（消息/channel 解析不出）：拒绝，不 fallback 放行
    #[test]
    fn bound_broken_binding_denied() {
        assert!(!authorize_file_access(2, 1, Some(99), None));
    }

    // bound 文件即使是 uploader，非成员也不能靠 uploader 身份绕过
    #[test]
    fn bound_uploader_but_non_member_denied() {
        assert!(!authorize_file_access(1, 1, Some(99), Some(false)));
    }

    // bound 文件 uploader 且是成员 → 允许
    #[test]
    fn bound_uploader_and_member_allowed() {
        assert!(authorize_file_access(1, 1, Some(99), Some(true)));
    }
}
