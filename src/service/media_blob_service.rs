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

//! 附件秒传：物理对象（blob）与逻辑句柄（file_upload）之间的那一层。
//!
//! 本产品没有「转发协议」。转发就是**当前用户重新发一条同样的消息**，
//! 唯一需要的能力是：同一份内容第二次发送时不再上传字节。
//!
//! 所以这里只做两件事：
//!   1. 上传收尾时，把这份字节登记成一个 blob（同内容 + 同处理版本只登记一次）；
//!   2. 下次有人要发同样的内容时，为**他自己**建一个新句柄指向同一个 blob。
//!
//! 关键在第 2 步的「他自己」：返回的是当前用户名下的新 `file_id`，不是别人的。
//! 后续发消息时的附件归属校验（`uploader_id = sender_id`）因此自然通过，
//! 不需要任何转发专用的提交分支。

use sqlx::PgPool;

use crate::error::{Result, ServerError};

/// 一个物理对象。
#[derive(Debug, Clone)]
pub struct MediaBlob {
    pub blob_id: i64,
    pub storage_path: String,
    pub storage_source_id: i32,
    pub file_size: i64,
    pub mime_type: String,
    pub encryption_version: i32,
    pub cek: Option<String>,
}

/// 秒传判定的身份：**内容摘要**，仅此而已。
///
/// 这里的「内容」是**压缩/转码之后、加密之前的明文最终字节**。加密用随机 nonce，
/// 同一份文件每次密文都不同，所以密文摘要不能当身份——那是上一版命中率恒为 0 的原因。
///
/// `transform_version` 只是元数据，**不参与身份**：字节不同摘要自然不同，
/// 字节相同就该复用。因为压缩器版本号不同而把同样的字节存两份，是白占存储。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobIdentity {
    pub content_sha256: String,
}

impl BlobIdentity {
    /// 摘要必须是 64 个十六进制字符（SHA-256）。
    ///
    /// 校验放在入口，是因为脏摘要不会立刻报错——它只会让秒传永远不命中，
    /// 表现成「怎么每次都重传」，很难往回查。
    pub fn parse(sha256: &str) -> Result<Self> {
        let normalized = sha256.trim().to_ascii_lowercase();
        if normalized.len() != 64 || !normalized.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ServerError::Validation(
                "sha256 必须是 64 位十六进制（SHA-256）".to_string(),
            ));
        }
        Ok(Self {
            content_sha256: normalized,
        })
    }
}

/// 登记一个物理对象；同内容 + 同处理版本已存在时返回已有的那个。
///
/// 用 `ON CONFLICT DO UPDATE ... RETURNING`（而不是先查后插）是因为两个用户可能
/// 同时上传同一份内容：先查后插会在并发下插出两行，唯一索引把后一个打成错误，
/// 而那次上传其实是成功的。
pub async fn register_blob(
    pool: &PgPool,
    identity: &BlobIdentity,
    // 服务端对**实际落盘字节**求的摘要（完整性用，不参与身份）。
    stored_sha256: &str,
    transform_version: i32,
    storage_path: &str,
    storage_source_id: i32,
    file_size: i64,
    mime_type: &str,
    encryption_version: i32,
    cek: Option<&str>,
) -> Result<MediaBlob> {
    let row: (i64, String, i32, i64, String, i32, Option<String>) = sqlx::query_as(
        r#"
        INSERT INTO privchat_media_blobs
            (content_sha256, stored_sha256, transform_version, storage_path,
             storage_source_id, file_size, mime_type, encryption_version, cek)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ON CONFLICT (content_sha256) DO UPDATE
            SET content_sha256 = EXCLUDED.content_sha256
        RETURNING blob_id, storage_path, storage_source_id, file_size, mime_type,
                  encryption_version, cek
        "#,
    )
    .bind(&identity.content_sha256)
    .bind(stored_sha256)
    .bind(transform_version)
    .bind(storage_path)
    .bind(storage_source_id)
    .bind(file_size)
    .bind(mime_type)
    .bind(encryption_version)
    .bind(cek)
    .fetch_one(pool)
    .await
    .map_err(|e| ServerError::Database(format!("登记物理对象失败: {e}")))?;

    Ok(MediaBlob {
        blob_id: row.0,
        storage_path: row.1,
        storage_source_id: row.2,
        file_size: row.3,
        mime_type: row.4,
        encryption_version: row.5,
        cek: row.6,
    })
}

/// 按内容找已有的物理对象。
pub async fn find_blob(pool: &PgPool, identity: &BlobIdentity) -> Result<Option<MediaBlob>> {
    let row: Option<(i64, String, i32, i64, String, i32, Option<String>)> = sqlx::query_as(
        r#"
        SELECT blob_id, storage_path, storage_source_id, file_size, mime_type,
               encryption_version, cek
        FROM privchat_media_blobs
        WHERE content_sha256 = $1
        "#,
    )
    .bind(&identity.content_sha256)
    .fetch_optional(pool)
    .await
    .map_err(|e| ServerError::Database(format!("查询物理对象失败: {e}")))?;

    Ok(row.map(
        |(blob_id, storage_path, storage_source_id, file_size, mime_type, encryption_version, cek)| {
            MediaBlob {
                blob_id,
                storage_path,
                storage_source_id,
                file_size,
                mime_type,
                encryption_version,
                cek,
            }
        },
    ))
}

/// 请求者能不能复用这个物理对象。
///
/// 🔴 只凭摘要就发句柄，等于「知道 hash 就等于拥有这个文件」。所以判据是
/// **他已经有权读到这份内容**：他自己上传过，或者他能读到某条引用了它的消息。
///
/// 这一条对转发是天然满足的——转发的人本来就在源会话里看到过这张图。
/// 换句话说，转发不需要任何额外授权协议，它复用的就是「你看得见才发得出」。
pub async fn may_reuse(
    pool: &PgPool,
    message_repository: &crate::repository::PgMessageRepository,
    channel_service: &crate::service::ChannelService,
    blob_id: i64,
    requester_id: u64,
) -> Result<bool> {
    let handles: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT file_id, uploader_id FROM privchat_file_uploads WHERE blob_id = $1",
    )
    .bind(blob_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ServerError::Database(format!("查询逻辑句柄失败: {e}")))?;

    for (file_id, uploader_id) in handles {
        if uploader_id as u64 == requester_id {
            return Ok(true);
        }
        let Some(meta) = crate::repository::FileUploadRepository::new(std::sync::Arc::new(
            pool.clone(),
        ))
        .get_by_file_id(file_id as u64)
        .await?
        else {
            continue;
        };
        // 判据只有一份：和 `file/get_url` 用的是同一个决策函数。
        // 在这里另写一套「大概等价」的判断，迟早会和它分叉。
        match crate::service::attachment_authorization::resolve_attachment_access(
            message_repository,
            channel_service,
            &meta,
            requester_id,
        )
        .await
        {
            Ok(decision) if decision.authorized => return Ok(true),
            Ok(_) => continue,
            // 🔴 判定不出来按**不放行**：秒传放宽的是「省一次上传」，
            // 放错的代价是把别人的文件发给了不该有的人。
            Err(_) => continue,
        }
    }

    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_digest_must_be_sixty_four_hex_characters() {
        assert!(BlobIdentity::parse(
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        )
        .is_ok());

        // 旧实现写的是 `hash:<u64>`；放进来只会让秒传永远不命中，必须当场拒绝。
        assert!(BlobIdentity::parse("hash:12345678901234567890").is_err());
        assert!(BlobIdentity::parse("abc").is_err());
        assert!(BlobIdentity::parse(&"z".repeat(64)).is_err());
    }

    #[test]
    fn digests_are_compared_case_insensitively() {
        let upper = BlobIdentity::parse(
            "D01F1B584BE7A9E4ACBAAC536ABFA9F00D9D33FB62A5CE76C54A25EE096908BD",
        )
        .expect("uppercase hex is still hex");
        let lower = BlobIdentity::parse(
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        )
        .expect("lowercase");
        assert_eq!(
            upper, lower,
            "同一份内容不能因为大小写写法不同就被当成两个对象",
        );
    }

    /// 处理版本**不**参与身份。
    ///
    /// 我上一版把它放进唯一键，理由是「换了压缩算法不能命中旧对象」——那条推理错了：
    /// 换算法产出的字节不同，摘要自然就不同，本来就命不中；而字节相同就该复用。
    /// 放进键里只会让同样的字节因为版本号不同被存两份。
    #[test]
    fn the_transform_version_is_not_part_of_the_identity() {
        let a = BlobIdentity::parse(
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        )
        .unwrap();
        let b = BlobIdentity::parse(
            "d01f1b584be7a9e4acbaac536abfa9f00d9d33fb62a5ce76c54a25ee096908bd",
        )
        .unwrap();
        assert_eq!(a, b, "同样的字节就是同一个对象，与谁产出的无关");
    }
}
