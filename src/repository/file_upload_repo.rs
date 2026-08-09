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

//! 文件上传记录仓库 - 持久化上传元数据到数据库（有据可查，清理不依赖缓存）

use crate::error::{Result, ServerError};
use crate::model::file_upload::{FileMetadata, FileType};
use sqlx::PgPool;
use std::sync::Arc;

/// 文件上传记录仓库
#[derive(Clone)]
pub struct FileUploadRepository {
    pool: Arc<PgPool>,
}

impl FileUploadRepository {
    pub fn new(pool: Arc<PgPool>) -> Self {
        Self { pool }
    }

    /// 底层连接池。删除时要在事务里锁住共享同一物理文件的行。
    pub fn pool(&self) -> &PgPool {
        self.pool.as_ref()
    }

    /// 秒传探测：这份内容在不在。**不写任何东西。**
    ///
    /// 判重只看**落盘字节**的 SHA-256：摘要相同即字节相同，大小自然相同，
    /// 也不存在「明文和密文互相复用」——字节都一样了，就是同一份东西。
    /// 摘要由服务端对实际收到那串字节计算；客户端 prepare 报的同名值只用于预检。
    pub async fn find_by_content(&self, sha256: &str) -> Result<Option<FileMetadata>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT file_id FROM privchat_file_uploads \
             WHERE file_hash = $1 ORDER BY file_id LIMIT 1",
        )
        .bind(sha256)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询同内容文件失败: {}", e)))?;

        match row {
            Some((file_id,)) => self.get_by_file_id(file_id as u64).await,
            None => Ok(None),
        }
    }

    /// 秒传取用：照着已有那行，给**当前用户**插一条新记录。
    ///
    /// 复制的是 `file_path` / 存储源 / 加密版本 / CEK —— 物理文件一份，两行指向它。
    /// 🔴 新行的 `uploader_id` 是请求者自己，`business_id` 留空：他要把这个文件绑到
    /// **他自己的**那条消息上。绝不返回别人的 `file_id`。
    pub async fn copy_for_user(
        &self,
        source: &FileMetadata,
        uploader_id: u64,
        business_type: &str,
    ) -> Result<u64> {
        let file_id = self.next_file_id().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启秒传取用事务失败: {}", e)))?;

        // 🔴 与删除**共用同一把 file_path 锁**。否则会出现：
        //   claim 读到源行 → delete 删掉最后一行并删物理文件 → claim 插入新行
        // 结果是一条指向已被删除文件的记录。加引用和减引用必须排成序。
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
            .bind(&source.file_path)
            .execute(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("获取物理文件锁失败: {}", e)))?;

        // 拿到锁之后复查源行还在不在：等锁期间它可能已经被删掉了。
        let still_there: Option<(i64,)> = sqlx::query_as(
            "SELECT file_id FROM privchat_file_uploads WHERE file_path = $1 LIMIT 1",
        )
        .bind(&source.file_path)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("复查源文件失败: {}", e)))?;
        if still_there.is_none() {
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(
                "该文件已被删除，请正常上传".to_string(),
            ));
        }

        sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_size, file_type, mime_type,
                file_path, storage_source_id, uploader_id, uploaded_at,
                width, height, file_hash, business_type, encryption_version, cek
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now_millis(), $9, $10, $11, $12, $13, $14)
            "#,
        )
        .bind(file_id as i64)
        .bind(&source.original_filename)
        .bind(source.file_size as i64)
        .bind(source.file_type.as_str())
        .bind(&source.mime_type)
        .bind(&source.file_path)
        .bind(source.storage_source_id as i32)
        .bind(uploader_id as i64)
        .bind(source.width.map(|v| v as i32))
        .bind(source.height.map(|v| v as i32))
        .bind(&source.file_hash)
        .bind(business_type)
        .bind(source.encryption_version)
        .bind(&source.cek)
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("创建秒传记录失败: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交秒传取用事务失败: {}", e)))?;
        Ok(file_id)
    }

    /// 除了这一行，还有没有别人指着同一个物理文件。
    ///
    /// 删除时用：还有人指着就只删数据库行，物理文件留着。
    pub async fn other_rows_share_path(&self, file_id: u64, file_path: &str) -> Result<bool> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM privchat_file_uploads WHERE file_path = $1 AND file_id <> $2",
        )
        .bind(file_path)
        .bind(file_id as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("统计共享同一物理文件的记录失败: {}", e)))?;
        Ok(count > 0)
    }

    /// 取下一个自增 file_id    /// 取下一个自增 file_id（BIGSERIAL 序列），用于先得到 id 再落盘、再 insert
    pub async fn next_file_id(&self) -> Result<u64> {
        let row: (i64,) = sqlx::query_as("SELECT nextval('privchat_file_uploads_id_seq')::BIGINT")
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
                business_type, business_id, encryption_version, cek
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
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
        .bind(meta.encryption_version)
        .bind(&meta.cek)
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
            encryption_version: i32,
            cek: Option<String>,
        }
        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT file_id, original_filename, file_size, file_type, mime_type,
                   file_path, storage_source_id, uploader_id, uploader_ip, uploaded_at, width, height, file_hash,
                   business_type, business_id, encryption_version, cek
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
            encryption_version: r.encryption_version,
            cek: r.cek,
        }))
    }

    /// 按业务类型+业务ID 查询 file_id 列表（便于随业务数据删除时清理）
    pub async fn list_file_ids_by_business(
        &self,
        business_type: &str,
        business_id: &str,
    ) -> Result<Vec<u64>> {
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
    pub async fn update_business(
        &self,
        file_id: u64,
        business_type: &str,
        business_id: &str,
    ) -> Result<bool> {
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
    /// 这个文件当前被多少条消息引用（含已撤回/已删除的引用）。
    ///
    /// 用于删除守卫：**被引用过的文件不允许按上传者意愿删除**。
    /// 共享引用上线后，一个上传者删掉自己的原图，会让所有转发副本一起损坏。
    pub async fn reference_count(&self, file_id: u64) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM privchat_message_file_refs WHERE file_id = $1",
        )
        .bind(file_id as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询文件引用数失败: {}", e)))?;
        Ok(count)
    }

    pub async fn delete(&self, file_id: u64) -> Result<bool> {
        let result = sqlx::query("DELETE FROM privchat_file_uploads WHERE file_id = $1")
            .bind(file_id as i64)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| ServerError::Database(format!("删除上传记录失败: {}", e)))?;
        Ok(result.rows_affected() > 0)
    }
}
