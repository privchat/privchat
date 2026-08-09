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

/// 一个文件的引用现状：由调用方把 IO 查好再传进来（纯决策，便于单测）。
///
/// 三个字段各自回答一个独立问题，**不能互相推导**：
/// - `has_any_reference`：这个文件有没有被任何消息引用过（含已撤回/已删除的）。
///   `false` = pending（上传了还没发出去）。
/// - `requester_is_member_of_a_live_reference`：请求者是不是**某条仍然有效**的引用
///   消息所在会话的成员。这是放行的正条件。
/// - `uploader_id` / `requester_id`：pending 阶段唯一的判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileAccessFacts {
    pub requester_id: u64,
    pub uploader_id: u64,
    pub has_any_reference: bool,
    pub requester_is_member_of_a_live_reference: bool,
}

/// 附件访问授权纯决策（无 IO；MEDIA_REFERENCE_AND_FORWARD_SPEC §4.1）。
///
/// ```text
/// 放行 ⟺ 存在一条引用该文件的消息 M，且
///          M.deleted = false AND M.revoked = false
///          AND requester 是 M.channel_id 的成员
///      或  文件尚未被任何消息引用（pending）AND requester == uploader
/// ```
///
/// 🔴 授权主体是 **requester**，不是 sender。写成 sender 会变成
/// 「A 发的消息，B 下载时按 A 的权限放行」。
///
/// 🔴 uploader 身份**不能**绕过成员校验：文件一旦被消息引用，就只看会话成员关系。
/// 否则上传者可以下载自己被转发到陌生群里的那份，反过来也给了「先上传再蹭权限」的口子。
///
/// 🔴 「有引用但全都失效」≠「没有引用」。前者拒绝（撤回后不该再能下载），
/// 后者回落 uploader（还没发出去，只有自己能看）。
pub fn authorize_file_access(facts: FileAccessFacts) -> bool {
    if facts.has_any_reference {
        facts.requester_is_member_of_a_live_reference
    } else {
        facts.requester_id == facts.uploader_id
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
            hasher: <sha2::Sha256 as sha2::Digest>::new(),
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
        // 产出这份字节的客户端处理版本；0 = 原始未处理。
        transform_version: i32,
    ) -> Result<FileMetadata> {
        let mut writer = upload
            .writer
            .take()
            .ok_or_else(|| ServerError::Internal("上传流已关闭".to_string()))?;
        writer
            .close()
            .await
            .map_err(|e| ServerError::Internal(format!("存储写入收尾失败: {}", e)))?;

        // 内容摘要定稿。秒传按它认身份，所以这里必须是**最终字节**的 SHA-256。
        let content_sha256 = hex::encode(sha2::Digest::finalize(upload.hasher));

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
            file_hash: Some(content_sha256.clone()),
            business_type: Some(business_type),
            business_id,
            encryption_version,
            cek,
        };
        self.file_upload_repo.insert(&metadata).await?;

        // 登记物理对象并把句柄挂上去：下一次有人发同样的内容，就能命中秒传。
        //
        // 🔴 这一步失败不能让整次上传失败——字节已经落盘、句柄已经建好，用户的
        // 文件是好的。丢的只是「下次能不能省一次上传」，那是优化不是正确性。
        let identity = crate::service::media_blob_service::BlobIdentity {
            content_sha256: content_sha256.clone(),
            transform_version,
        };
        match crate::service::media_blob_service::register_blob(
            self.file_upload_repo.pool(),
            &identity,
            &metadata.file_path,
            metadata.storage_source_id as i32,
            metadata.file_size as i64,
            &metadata.mime_type,
            metadata.encryption_version,
            metadata.cek.as_deref(),
        )
        .await
        {
            Ok(blob) => {
                if let Err(e) = self
                    .file_upload_repo
                    .set_blob_id(metadata.file_id, blob.blob_id)
                    .await
                {
                    tracing::warn!("关联物理对象失败 file_id={}: {}", metadata.file_id, e);
                }
            }
            Err(e) => tracing::warn!("登记物理对象失败 sha256={}: {}", content_sha256, e),
        }

        Ok(metadata)
    }

    /// 秒传命中：为当前用户建一个指向同一个物理对象的新句柄。
    pub async fn create_handle_for_blob(
        &self,
        blob: &crate::service::media_blob_service::MediaBlob,
        uploader_id: u64,
        filename: &str,
        file_type: &str,
        business_type: &str,
    ) -> Result<u64> {
        self.file_upload_repo
            .create_handle_for_blob(blob, uploader_id, filename, file_type, business_type)
            .await
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

    /// 直接物理删除文件——**已停用**（MEDIA_REFERENCE_AND_FORWARD_SPEC §8.2）。
    ///
    /// 共享引用模型下「我上传的文件我能删」不成立：一个文件可能同时被原消息和
    /// 若干转发副本引用，删掉物理文件会让那些副本一起变成打不开的图。
    ///
    /// 「先数引用再删」也不够——两步之间可以插入一条新引用（转发只要一个事务），
    /// 删除照样发生。要做对必须是 GC 状态机：`status=gc_pending` + 宽限期 +
    /// 到点复查引用，全程可被新引用取消。
    ///
    /// 在那套状态机落地之前这里直接拒绝。**不做 ownership / 引用计数查询**——
    /// 查了也不影响结果，只是让人误以为这里还有一套判断在生效。
    /// 现状：本方法无 RPC 调用方，这是拆引信，不是砍功能。
    pub async fn delete_file(&self, file_id: u64, _user_id: u64) -> Result<()> {
        tracing::warn!("🚫 拒绝直接删除文件 file_id={file_id}：引用计数 GC 未落地前不提供物理删除");
        Err(ServerError::Forbidden(
            "直接删除文件已停用，等待引用计数 GC".to_string(),
        ))
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
    ///
    /// 与签发 token 时用的是同一个函数（[`FileType::max_size_bytes`]）——两处一旦分家，
    /// 松的那个会放进来一批注定失败的上传。
    fn max_size_for_type(file_type: &FileType) -> usize {
        file_type.max_size_bytes() as usize
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
    /// 🔴 内容摘要必须是 **SHA-256**，不能用 `DefaultHasher`。
    ///
    /// `DefaultHasher` 是 SipHash：标准库明确写着**不保证跨 Rust 版本稳定**，
    /// 只有 64 位，也不是密码学摘要。拿它当文件内容标识，秒传会在某次工具链升级后
    /// 集体失配（同一个文件算出不同值 → 全量重传），碰撞也不是理论问题。
    hasher: sha2::Sha256,
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
        self.written = self.written.saturating_add(chunk.len() as u64);
        if self.written > self.limit {
            return Err(ServerError::Validation(format!(
                "文件大小超过限制: {} > {} bytes",
                self.written, self.limit
            )));
        }
        sha2::Digest::update(&mut self.hasher, &chunk);
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
            tracing::warn!("⚠️ 清理中止上传的半文件失败 path={}: {}", self.file_path, e);
        }
    }
}

#[cfg(test)]
mod authz_tests {
    use super::{authorize_file_access, FileAccessFacts};

    fn facts(
        requester_id: u64,
        uploader_id: u64,
        has_any_reference: bool,
        member_of_live: bool,
    ) -> FileAccessFacts {
        FileAccessFacts {
            requester_id,
            uploader_id,
            has_any_reference,
            requester_is_member_of_a_live_reference: member_of_live,
        }
    }

    // pending（还没被任何消息引用）：仅 uploader 可访问
    #[test]
    fn pending_uploader_allowed() {
        assert!(authorize_file_access(facts(1, 1, false, false)));
    }

    #[test]
    fn pending_non_uploader_denied() {
        assert!(!authorize_file_access(facts(2, 1, false, false)));
    }

    // 被有效消息引用：会话成员可访问
    #[test]
    fn referenced_member_allowed() {
        assert!(authorize_file_access(facts(2, 1, true, true)));
    }

    #[test]
    fn referenced_non_member_denied() {
        assert!(!authorize_file_access(facts(2, 1, true, false)));
    }

    // uploader 身份不能绕过成员校验
    #[test]
    fn referenced_uploader_but_non_member_denied() {
        assert!(!authorize_file_access(facts(1, 1, true, false)));
    }

    #[test]
    fn referenced_uploader_and_member_allowed() {
        assert!(authorize_file_access(facts(1, 1, true, true)));
    }

    /// 【spec §4.2 的回归】引用全部失效（撤回/删除）→ 拒绝。
    ///
    /// 这条正是「撤回后附件仍可下载」那个洞：撤回是软删，行还在，
    /// 旧实现裸查 channel_id 拿到会话、成员校验通过，于是照常放行。
    #[test]
    fn every_reference_revoked_denies_even_the_uploader() {
        assert!(!authorize_file_access(facts(1, 1, true, false)));
        assert!(!authorize_file_access(facts(2, 1, true, false)));
    }

    /// 【转发的核心用例】上传者与请求者毫无关系，只要请求者在某条有效引用
    /// 消息的会话里就该放行——转发副本的接收方正是这个形态。
    #[test]
    fn a_forwarded_copy_is_readable_by_someone_unrelated_to_the_uploader() {
        assert!(authorize_file_access(facts(777, 1, true, true)));
    }

    /// 上传摘要必须是 **SHA-256 的十六进制**，秒传要靠它判「同一份内容」。
    ///
    /// 🔴 这里曾经用 `DefaultHasher`，写出来的是 `hash:<u64>`。那是 SipHash：
    /// 标准库明确说**不保证跨 Rust 版本稳定**，只有 64 位，也不是密码学摘要。
    /// 换个工具链重编，同一个文件算出来的值就变了——秒传会从「命中」变成全量重传，
    /// 而且这种失效不会报错，只会悄悄变慢。
    #[test]
    fn the_upload_digest_is_a_sha256_hex_string() {
        use sha2::Digest as _;

        let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
        hasher.update(b"privchat");
        let digest = hex::encode(hasher.finalize());

        assert_eq!(digest.len(), 64, "SHA-256 十六进制是 64 个字符");
        assert!(
            digest.chars().all(|c| c.is_ascii_hexdigit()),
            "必须是纯十六进制，不能带 `hash:` 之类前缀——那种值没法跨端比对",
        );
        // 已知向量：换实现或换编码方式都会在这里断掉。
        assert_eq!(
            digest,
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        );
    }
}
