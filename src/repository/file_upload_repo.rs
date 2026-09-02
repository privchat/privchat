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
use crate::model::file_upload::{
    AttachmentObject, FileMetadata, FileType,
};
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

    /// 秒传预检：按**明文摘要**找物理对象。**不写任何东西。**
    ///
    /// 🔴 返回的是对象，不是别人的引用行。claim 只需要 `object_id`——引用行上那些
    /// 文件名、MIME 是第一个上传者的私事，不该出现在这条路径上。
    pub async fn find_object_by_plaintext_sha256(
        &self,
        plaintext_sha256: &str,
    ) -> Result<Option<AttachmentObject>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            object_id: i64,
            plaintext_sha256: String,
            plaintext_size: i64,
            sealed_sha256: String,
            sealed_size: i64,
            file_path: String,
            storage_source_id: i32,
            format_version: i16,
            encryption_key_id: i16,
        }
        let row = sqlx::query_as::<_, Row>(
            "SELECT object_id, plaintext_sha256, plaintext_size, sealed_sha256, sealed_size, \
                    file_path, storage_source_id, format_version, encryption_key_id \
             FROM privchat_attachment_objects WHERE plaintext_sha256 = $1",
        )
        .bind(plaintext_sha256)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("按明文摘要查对象失败: {e}")))?;

        Ok(row.map(|r| AttachmentObject {
            object_id: r.object_id as u64,
            plaintext_sha256: r.plaintext_sha256,
            plaintext_size: r.plaintext_size as u64,
            sealed_sha256: r.sealed_sha256,
            sealed_size: r.sealed_size as u64,
            file_path: r.file_path,
            storage_source_id: r.storage_source_id as u32,
            format_version: r.format_version as u8,
            encryption_key_id: r.encryption_key_id as u8,
        }))
    }

    /// 这个幂等键之前是不是已经成功取用过。
    ///
    /// 命中就直接把当时那个 `file_id` 还回去——响应丢了、客户端重试，拿到的仍是
    /// 同一份，而不是又多一行。
    pub async fn find_claimed(&self, uploader_id: u64, claim_key_hash: &str) -> Result<Option<u64>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT file_id FROM privchat_file_uploads \
             WHERE uploader_id = $1 AND claim_key_hash = $2",
        )
        .bind(uploader_id as i64)
        .bind(claim_key_hash)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("查询秒传幂等记录失败: {}", e)))?;
        Ok(row.map(|(id,)| id as u64))
    }

    /// 锁等待超时（PostgreSQL `55P03`）是**瞬时**竞争，不是这次取用不合法。
    ///
    /// 包成 Database 的话它会一路落成 internal，客户端把一次锁竞争当成永久失败，
    /// 附件就再也发不出去了。映射成 ServiceUnavailable，让上层照常重试。
    /// 🔴 `55P03`（`lock_not_available`）是**瞬时竞争**，不是数据库故障：包成
    /// `Database` 就成了终局失败，一次并发让这条附件永远发不出去。
    ///
    /// 收敛与建引用共用这一处映射——同一类超时在不同上传路径上不能表现成不同错误。
    pub(crate) fn map_lock_error(context: &str, e: sqlx::Error) -> ServerError {
        if let Some(db) = e.as_database_error() {
            if db.code().as_deref() == Some("55P03") {
                return ServerError::ServiceUnavailable(format!("{context}: 锁等待超时，请重试"));
            }
        }
        ServerError::Database(format!("{context}: {e}"))
    }

}

/// 建立引用时要写的**逻辑**元数据。
///
/// 🔴 全部来自**当前**这次 claim 的 token，一个字段都不从源记录复制。
///
/// 以前是照着源记录逐列复制 `original_filename` / `mime_type` / 宽高的。那是隐私泄露：
/// 第一个人上传 `离婚协议-张三李四.pdf`，第二个人按摘要秒传同一串字节之后，
/// 拿到的记录上写着第一个人的文件名。物理对象能提供的只有 `object_id`；
/// 「这个文件叫什么、是什么类型、属于哪条业务」是当前用户自己的事。
pub struct ReferenceMetadata<'a> {
    pub original_filename: &'a str,
    pub file_type: &'a FileType,
    pub mime_type: &'a str,
    pub business_type: &'a str,
}

impl FileUploadRepository {
    /// 秒传取用：让**当前用户**多持有一条指向既有物理对象的引用。
    ///
    /// 🔴 这里**不做任何授权判断**。判据在调用方（`file_claim_service`）：持有明文
    /// SHA-256 即视为持有该内容。以前这个函数里还有一整套"申请者对源记录所在的消息/
    /// 频道/群是否有权"的复查，那条规则让跨用户秒传根本不成立——两个互不相识的人发
    /// 同一份文件时，第二个人对第一个人的记录当然没有访问权，于是必然退回整传。
    ///
    /// 它同时还在按 `file_hash` 查表，而那一列已经不存在了；SQL 不受编译器检查，
    /// 所以只会在运行到这条路径时才炸。
    pub async fn create_reference(
        &self,
        object_id: u64,
        uploader_id: u64,
        meta: &ReferenceMetadata<'_>,
        // 幂等键（token 的摘要）。与那一行**同事务**写入，所以「插进去了」
        // 和「记下取用过了」不可能只发生一半。
        claim_key_hash: Option<&str>,
    ) -> Result<u64> {
        let file_id = self.next_file_id().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启秒传取用事务失败: {e}")))?;

        // 🔴 等待上限必须在**第一把锁之前**设，否则下面那把 advisory 锁仍可无限等，
        // 「封顶 3 秒」就是句空话。
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("设置锁等待上限失败: {e}")))?;

        // 🔴 与删除**共用同一把对象锁**。否则会出现：
        //   claim 读到对象 → GC 删掉最后一条引用并删物理文件 → claim 插入新引用
        // 结果是一条指向已被删除对象的记录。加引用和减引用必须排成序。
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(object_id as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| Self::map_lock_error("获取物理对象锁失败", e))?;

        // 拿到锁之后复查对象还在不在：等锁期间它可能已经被 GC 掉了。
        let still_there: Option<(i64,)> =
            sqlx::query_as("SELECT object_id FROM privchat_attachment_objects WHERE object_id = $1")
                .bind(object_id as i64)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| ServerError::Database(format!("复查物理对象失败: {e}")))?;
        if still_there.is_none() {
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(
                "服务端没有这份内容，请正常上传".to_string(),
            ));
        }

        sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_type, mime_type,
                object_id, uploader_id, uploaded_at, business_type, claim_key_hash
            ) VALUES ($1, $2, $3, $4, $5, $6, now_millis(), $7, $8)
            ON CONFLICT (uploader_id, claim_key_hash) WHERE claim_key_hash IS NOT NULL
            DO NOTHING
            "#,
        )
        .bind(file_id as i64)
        .bind(meta.original_filename)
        .bind(meta.file_type.as_str())
        .bind(meta.mime_type)
        .bind(object_id as i64)
        .bind(uploader_id as i64)
        .bind(meta.business_type)
        .bind(claim_key_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("创建秒传记录失败: {e}")))?;

        // 唯一索引把并发的第二个 claim 挡成 0 行：读回先到那个的 file_id 返回，
        // 而不是报错。两个人拿同一个 token 重试，应该拿到同一份，不是一个成功一个失败。
        let file_id = if let Some(key) = claim_key_hash {
            let row: Option<(i64,)> = sqlx::query_as(
                "SELECT file_id FROM privchat_file_uploads \
                 WHERE uploader_id = $1 AND claim_key_hash = $2",
            )
            .bind(uploader_id as i64)
            .bind(key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("回读秒传记录失败: {e}")))?;
            row.map(|(id,)| id as u64).unwrap_or(file_id)
        } else {
            file_id
        };

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交秒传取用事务失败: {e}")))?;
        Ok(file_id)
    }

    /// 除了这一行，还有没有别人指着同一个物理对象。
    ///
    /// 删除时用：还有人指着就只删引用行，物理对象留着。
    ///
    /// 🔴 现查现算，**不维护 `reference_count` 列**：那种计数在异常重试与事务回滚下
    /// 会漂移，而漂移的方向恰好是"以为还有人用"（泄漏）或"以为没人用了"（删掉别人
    /// 还在用的文件）。外键在，`count(*)` 就是准的。
    pub async fn other_rows_share_object(&self, file_id: u64, object_id: u64) -> Result<bool> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT count(*) FROM privchat_file_uploads WHERE object_id = $1 AND file_id <> $2",
        )
        .bind(object_id as i64)
        .bind(file_id as i64)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("统计共享同一物理对象的记录失败: {}", e)))?;
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

    /// 插入一条**引用**记录。物理对象必须已经存在（`meta.object.object_id`）。
    pub async fn insert(&self, meta: &FileMetadata) -> Result<()> {
        sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_type, mime_type,
                object_id, uploader_id, uploader_ip, uploaded_at, width, height,
                business_type, business_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
            "#
        )
        .bind(meta.file_id as i64)
        .bind(&meta.original_filename)
        .bind(meta.file_type.as_str())
        .bind(&meta.mime_type)
        .bind(meta.object.object_id as i64)
        .bind(meta.uploader_id as i64)
        .bind(&meta.uploader_ip)
        .bind(meta.uploaded_at as i64)
        .bind(meta.width.map(|w| w as i32))
        .bind(meta.height.map(|h| h as i32))
        .bind(&meta.business_type)
        .bind(&meta.business_id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("插入上传记录失败: {}", e)))?;
        Ok(())
    }

    /// 按 file_id 查询（自己开连接）。
    pub async fn get_by_file_id(&self, file_id: u64) -> Result<Option<FileMetadata>> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| ServerError::Database(format!("获取查询连接失败: {e}")))?;
        Self::get_by_file_id_within(&mut conn, file_id).await
    }

    /// 按 file_id 查询，**在调用方的事务里**。
    ///
    /// 🔴 事务中间去 pool 上另开一条连接读，读的是另一个快照：
    ///   · 本事务刚写进去的行，它看不见；
    ///   · 更要命的是主键冲突后的回读——冲突说明那一行**就在某个事务里**，
    ///     从池外读到的可能是它提交前的样子，也可能干脆读不到，
    ///     于是"冲突却读不到"这种自相矛盾的内部错误就冒出来了。
    ///   · 池被本事务占满时，再要一条连接还会死等。
    ///
    /// 判定身份要用和写入同一个快照，所以这条必须收在事务里跑。
    pub async fn get_by_file_id_within(
        conn: &mut sqlx::PgConnection,
        file_id: u64,
    ) -> Result<Option<FileMetadata>> {
        #[derive(sqlx::FromRow)]
        struct Row {
            file_id: i64,
            original_filename: String,
            file_type: String,
            mime_type: String,
            uploader_id: i64,
            uploader_ip: Option<String>,
            uploaded_at: i64,
            width: Option<i32>,
            height: Option<i32>,
            business_type: Option<String>,
            business_id: Option<String>,
            object_id: i64,
            plaintext_sha256: String,
            plaintext_size: i64,
            sealed_sha256: String,
            sealed_size: i64,
            file_path: String,
            storage_source_id: i32,
            format_version: i16,
            encryption_key_id: i16,
        }
        // 🔴 物理事实一律从对象表读。引用行上已经没有这些列了（migration 032），
        // 所以这条 JOIN 不是优化，是唯一的读法。
        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT u.file_id, u.original_filename, u.file_type, u.mime_type,
                   u.uploader_id, u.uploader_ip, u.uploaded_at, u.width, u.height,
                   u.business_type, u.business_id,
                   o.object_id, o.plaintext_sha256, o.plaintext_size,
                   o.sealed_sha256, o.sealed_size,
                   o.file_path, o.storage_source_id, o.format_version, o.encryption_key_id
            FROM privchat_file_uploads u
            JOIN privchat_attachment_objects o ON o.object_id = u.object_id
            WHERE u.file_id = $1
            "#
        )
        .bind(file_id as i64)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| ServerError::Database(format!("查询上传记录失败: {}", e)))?;

        Ok(row.map(|r| FileMetadata {
            file_id: r.file_id as u64,
            original_filename: r.original_filename,
            original_size: None,
            file_type: FileType::from_str(&r.file_type).unwrap_or(FileType::Other),
            mime_type: r.mime_type,
            uploader_id: r.uploader_id as u64,
            uploader_ip: r.uploader_ip,
            uploaded_at: r.uploaded_at as u64,
            width: r.width.map(|w| w as u32),
            height: r.height.map(|h| h as u32),
            business_type: r.business_type,
            business_id: r.business_id,
            object: AttachmentObject {
                object_id: r.object_id as u64,
                plaintext_sha256: r.plaintext_sha256,
                plaintext_size: r.plaintext_size as u64,
                sealed_sha256: r.sealed_sha256,
                sealed_size: r.sealed_size as u64,
                file_path: r.file_path,
                storage_source_id: r.storage_source_id as u32,
                format_version: r.format_version as u8,
                encryption_key_id: r.encryption_key_id as u8,
            },
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
