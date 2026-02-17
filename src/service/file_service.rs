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

    pub async fn upload_file(
        &self,
        file_data: Vec<u8>,
        filename: String,
        mime_type: String,
        uploader_id: u64,
        uploader_ip: Option<String>,
        business_type: String,
        business_id: Option<String>,
    ) -> Result<FileMetadata> {
        let file_type = self.detect_file_type(&mime_type)?;
        self.validate_file_size(&file_data, &file_type)?;

        let source = self.resolve_storage_source()?;
        let op = self
            .operators
            .read()
            .await
            .get(&source.id)
            .cloned()
            .ok_or_else(|| {
                ServerError::Internal(format!("未找到存储源 id={} 的 Operator", source.id))
            })?;

        let file_id = self.file_upload_repo.next_file_id().await?;
        let file_path = self.generate_file_path(file_id, &file_type, &filename);
        let file_hash = self.calculate_file_hash(&file_data).await?;
        let file_size = file_data.len() as u64;

        op.write(&file_path, file_data)
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入失败: {}", e)))?;

        let metadata = FileMetadata {
            file_id,
            original_filename: filename,
            file_size,
            original_size: None,
            file_type,
            mime_type,
            file_path: file_path.clone(),
            storage_source_id: source.id,
            uploader_id,
            uploader_ip,
            uploaded_at: chrono::Utc::now().timestamp_millis() as u64,
            width: None,
            height: None,
            file_hash: Some(file_hash),
            business_type: Some(business_type),
            business_id,
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
        if mime_type.starts_with("image/") {
            Ok(FileType::Image)
        } else if mime_type.starts_with("video/") {
            Ok(FileType::Video)
        } else if mime_type.starts_with("audio/") {
            Ok(FileType::Audio)
        } else {
            Ok(FileType::File)
        }
    }

    fn validate_file_size(&self, file_data: &[u8], file_type: &FileType) -> Result<()> {
        let max_size = match file_type {
            FileType::Image => 10 * 1024 * 1024,
            FileType::Video => 100 * 1024 * 1024,
            FileType::Audio => 10 * 1024 * 1024,
            FileType::File => 50 * 1024 * 1024,
            FileType::Other => 10 * 1024 * 1024,
        };
        if file_data.len() > max_size {
            return Err(ServerError::Validation(format!(
                "文件大小超过限制: {} > {}",
                file_data.len(),
                max_size
            )));
        }
        Ok(())
    }

    fn generate_file_path(&self, file_id: u64, file_type: &FileType, filename: &str) -> String {
        let extension = filename.split('.').last().unwrap_or("bin");
        let subdir = match file_type {
            FileType::Image => "images",
            FileType::Video => "videos",
            FileType::Audio => "audios",
            FileType::File => "files",
            FileType::Other => "others",
        };
        format!("{}/{}.{}", subdir, file_id, extension)
    }

    pub async fn get_file_url(&self, file_id: u64, user_id: u64) -> Result<FileUrlResponse> {
        let metadata = self
            .get_file_metadata(file_id)
            .await?
            .ok_or_else(|| ServerError::NotFound("文件不存在".to_string()))?;
        if metadata.uploader_id != user_id {
            return Err(ServerError::Forbidden("无权访问此文件".to_string()));
        }
        let file_url = self.build_access_url(&metadata.file_path, metadata.storage_source_id);
        let expires_at = Utc::now().timestamp() + 3600 * 24 * 365;
        Ok(FileUrlResponse {
            file_url,
            thumbnail_url: None,
            expires_at,
            file_size: metadata.file_size,
            mime_type: metadata.mime_type,
            storage_source_id: metadata.storage_source_id,
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

    async fn calculate_file_hash(&self, file_data: &[u8]) -> Result<String> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        file_data.hash(&mut hasher);
        Ok(format!("hash:{}", hasher.finish()))
    }

    pub fn build_access_url(&self, file_path: &str, storage_source_id: u32) -> String {
        if let Some(src) = self.sources_by_id.get(&storage_source_id) {
            if let Some(base_url) = &src.base_url {
                let base = base_url.trim_end_matches('/');
                return if src.storage_type == "s3" {
                    format!("{}/{}", base, file_path)
                } else {
                    format!("{}/api/app/files/{}", base, file_path)
                };
            }
            return if src.storage_type == "s3" {
                format!("/{}", file_path)
            } else {
                format!("/api/app/files/{}", file_path)
            };
        }
        format!("{{unsupported:storage_source_id={}}}", storage_source_id)
    }
}
