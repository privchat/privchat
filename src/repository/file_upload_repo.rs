//! 文件上传记录仓库 - 持久化上传元数据到数据库（有据可查，清理不依赖缓存）

use std::sync::Arc;
use sqlx::PgPool;
use crate::error::{Result, ServerError};
use crate::model::file_upload::{FileMetadata, FileType};

/// 文件上传记录仓库
#[derive(Clone)]
pub struct FileUploadRepository {
    pool: Arc<PgPool>,
}

impl FileUploadRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 取下一个自增 file_id（BIGSERIAL 序列），用于先得到 id 再落盘、再 insert
    pub async fn next_file_id(&self) -> Result<u64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT nextval('privchat_file_uploads_id_seq')::BIGINT"
        )
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("获取 next file_id 失败: {}", e)))?;
        Ok(row.0 as u64)
    }

    /// 插入一条上传记录（file_id 已由 next_file_id 取得并用于生成 file_path）
    pub async fn insert(&self, meta: &FileMetadata) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_size, file_type, mime_type,
                file_path, storage_source_id, uploader_id, uploader_ip, uploaded_at, width, height, file_hash,
                business_type, business_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
            "#
        )
        .bind(meta.file_id as i64)
        .bind(&meta.original_filename)
        .bind(meta.file_size as i64)
        .bind(meta.file_type.as_str())
        .bind(&meta.mime_type)
        .bind(&meta.file_path)
        .bind(meta.storage_source_id as i32)
        .bind(meta.uploader_id as i64)
        .bind(&meta.uploader_ip)
        .bind(meta.uploaded_at as i64)
        .bind(meta.width.map(|w| w as i32))
        .bind(meta.height.map(|h| h as i32))
        .bind(&meta.file_hash)
        .bind(&meta.business_type)
        .bind(&meta.business_id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("插入上传记录失败: {}", e)))?;
        Ok(())
    }

    /// 按 file_id 查询
    pub async fn get_by_file_id(&self, file_id: u64) -> Result<Option<FileMetadata>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            file_id: i64,
            original_filename: String,
            file_size: i64,
            file_type: String,
            mime_type: String,
            file_path: String,
            storage_source_id: i32,
            uploader_id: i64,
            uploader_ip: Option<String>,
            uploaded_at: i64,
            width: Option<i32>,
            height: Option<i32>,
            file_hash: Option<String>,
            business_type: Option<String>,
            business_id: Option<String>,
        }
        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT file_id, original_filename, file_size, file_type, mime_type,
                   file_path, storage_source_id, uploader_id, uploader_ip, uploaded_at, width, height, file_hash,
                   business_type, business_id
            FROM privchat_file_uploads WHERE file_id = $1
            "#
        )
        .bind(file_id as i64)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询上传记录失败: {}", e)))?;

        Ok(row.map(|r| FileMetadata {
            file_id: r.file_id as u64,
            original_filename: r.original_filename,
            file_size: r.file_size as u64,
            original_size: None,
            file_type: FileType::from_str(&r.file_type).unwrap_or(FileType::Other),
            mime_type: r.mime_type,
            file_path: r.file_path,
            storage_source_id: r.storage_source_id as u32,
            uploader_id: r.uploader_id as u64,
            uploader_ip: r.uploader_ip,
            uploaded_at: r.uploaded_at as u64,
            width: r.width.map(|w| w as u32),
            height: r.height.map(|h| h as u32),
            file_hash: r.file_hash,
            business_type: r.business_type,
            business_id: r.business_id,
        }))
    }

    /// 按业务类型+业务ID 查询 file_id 列表（便于随业务数据删除时清理）
    pub async fn list_file_ids_by_business(&self, business_type: &str, business_id: &str) -> Result<Vec<u64>> {
        let rows = sqlx::query_scalar::<_, i64>(
            "SELECT file_id FROM privchat_file_uploads WHERE business_type = $1 AND business_id = $2"
        )
        .bind(business_type)
        .bind(business_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("按业务查询上传记录失败: {}", e)))?;
        Ok(rows.into_iter().map(|id| id as u64).collect())
    }

    /// 更新文件的业务关联（如消息发送后设置 message_id）
    pub async fn update_business(&self, file_id: u64, business_type: &str, business_id: &str) -> Result<bool> {
        let result = sqlx::query(
            "UPDATE privchat_file_uploads SET business_type = $1, business_id = $2 WHERE file_id = $3"
        )
        .bind(business_type)
        .bind(business_id)
        .bind(file_id as i64)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("更新上传记录业务关联失败: {}", e)))?;
        Ok(result.rows_affected() > 0)
    }

    /// 按 file_id 删除
    pub async fn delete(&self, file_id: u64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
            .bind(file_id as i64)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| ServerError::Database(format!("删除上传记录失败: {}", e)))?;
        Ok(result.rows_affected() > 0)
    }
}
