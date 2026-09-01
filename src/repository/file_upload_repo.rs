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

    /// 同一份内容的所有逻辑记录（有上限）。
    ///
    /// 一串字节可能已经被好几个用户各自记过一笔——物理对象一份，引用记录多条。
    /// 判「这个人能不能取用」时必须逐条看：他有权读的可能不是最老那条。
    ///
    /// 🔴 判重键是 `dedup_id`（明文摘要的 HMAC），不是密文摘要。每个对象有自己的
    /// 随机 salt，同一份明文由不同人封装会产出不同密文——按密文判重等于秒传只对
    /// 「自己重发自己」生效。
    pub async fn find_all_by_dedup_id(&self, dedup_id: &str) -> Result<Vec<FileMetadata>> {
        let ids: Vec<(i64,)> = sqlx::query_as(
            "SELECT u.file_id FROM privchat_file_uploads u \
             JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
             WHERE o.dedup_id = $1 AND o.status = 'published' \
             ORDER BY u.file_id LIMIT 16",
        )
        .bind(dedup_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| ServerError::Database(format!("按内容查所有记录失败: {}", e)))?;
        // 逐条取完整 metadata：`get_by_file_id` 是 file 表读取的唯一入口，
        // 列的映射只维护一份。上限 16 条，N+1 的量是有界的。
        let mut out = Vec::with_capacity(ids.len());
        for (file_id,) in ids {
            if let Some(meta) = self.get_by_file_id(file_id as u64).await? {
                out.push(meta);
            }
        }
        Ok(out)
    }

    /// 秒传探测：这份内容在不在。**不写任何东西。**
    ///
    /// 🔴 只认 `status = 'published'` 的对象。`pending` 的还没通过首传校验——
    /// 让它被命中就等于「先发布再校验」：一份没验过的对象进了索引，后来者会拿到它。
    pub async fn find_by_dedup_id(&self, dedup_id: &str) -> Result<Option<FileMetadata>> {
        let row: Option<(i64,)> = sqlx::query_as(
            "SELECT u.file_id FROM privchat_file_uploads u \
             JOIN privchat_attachment_objects o ON o.object_id = u.object_id \
             WHERE o.dedup_id = $1 AND o.status = 'published' \
             ORDER BY u.file_id LIMIT 1",
        )
        .bind(dedup_id)
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
    fn map_lock_error(context: &str, e: sqlx::Error) -> ServerError {
        if let Some(db) = e.as_database_error() {
            if db.code().as_deref() == Some("55P03") {
                return ServerError::ServiceUnavailable(format!("{context}: 锁等待超时，请重试"));
            }
        }
        ServerError::Database(format!("{context}: {e}"))
    }

    pub async fn copy_for_user(
        &self,
        source: &FileMetadata,
        uploader_id: u64,
        business_type: &str,
        // 幂等键（token 的摘要）。与那一行**同事务**写入，所以「插进去了」
        // 与「记下取用过」是同一件事，不存在中间态。
        claim_key_hash: Option<&str>,
    ) -> Result<u64> {
        // 🔴 秒传取用的身份是**对象**，不是摘要字符串。
        //
        // 以前按 `file_hash` 匹配，缺了摘要就得用空串顶上，于是匹配会落到
        // "所有没有摘要的记录"上——那是拿授权去赌一个空值。现在直接拿 object_id：
        // 它是外键，不存在"匹配到一堆"的可能。
        let object_id = source.object.object_id as i64;

        // 只有通过首传校验的对象可以被取用。pending 还没验过，legacy 是废止格式。
        if !source.object.status.is_usable() {
            return Err(ServerError::NotFound(
                "服务端没有这份内容，请正常上传".to_string(),
            ));
        }

        let file_id = self.next_file_id().await?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| ServerError::Database(format!("开启秒传取用事务失败: {}", e)))?;

        // 🔴 与删除**共用同一把 file_path 锁**。否则会出现：
        //   claim 读到源行 → delete 删掉最后一行并删物理文件 → claim 插入新行
        // 结果是一条指向已被删除文件的记录。加引用和减引用必须排成序。
        // 🔴 等待上限必须在**第一把锁之前**设。放在后面的话，最先取的这把
        // file_path advisory 锁仍然可以无限等——「封顶 3 秒」就是句空话。
        sqlx::query("SET LOCAL lock_timeout = '3s'")
            .execute(&mut *tx)
            .await
            .map_err(|e| ServerError::Database(format!("设置锁等待上限失败: {}", e)))?;

        // 锁的粒度是物理对象。用 object_id 而不是路径字符串：同一个对象只有一个 id，
        // 而路径是它的属性，将来搬运存储源会变。
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(object_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| Self::map_lock_error("获取物理文件锁失败", e))?;

        // 🔴 在**事务内、对数据库**再确认一次调用者此刻仍有权读这份内容。
        //
        // 两个理由，各自都足够：
        //
        // 1. claim 是一次**新的授权动作**。「撤回收不回已经下载的东西」不等于
        //    「撤回之后还能继续开新的 file_id」——后者是在失权之后继续授予。
        // 2. 规范判据 `resolve_attachment_access` 的成员部分走 ChannelService，
        //    而它命中内存缓存就直接返回（`get_channel_members`）。那份缓存陈旧多久，
        //    窗口就有多长，不是微秒级。这里直查库，绕开缓存。
        //
        // 这道闸**只能拒、不能放行**：规范判据仍是 `authorize_file_access`，
        // 这里只负责确认它依据的事实没有在期间变过。两者的状态对应关系由
        // `the_guard_agrees_with_the_canonical_rule` 钉住。
        // 🔴 授权复查必须**锁住**它依据的那几行，否则「在事务里查一次」只是查得晚
        // 一点：READ COMMITTED 下，撤回或退群完全可以在这条 SELECT 之后、INSERT
        // 之前提交。
        //
        // 锁序固定为 advisory(file_path) → message → channel，到此为止。
        // **绝不再往下锁成员行**：退群的顺序是 member(写) → AFTER trigger → channel(写)，
        // 再去锁 member 就成了环。成员状态改用「拿到频道锁之后新起一条语句重读」，
        // 见下方。撤回、移出成员、退群都是对这些行的
        // UPDATE/DELETE，会自然等在共享锁上；最终语义是二者必有一个先提交：
        //   claim 先提交 → 取用成功，随后撤回；
        //   撤回先提交 → claim 等到锁后重新判定，拒绝。
        //
        // 等待封顶，免得一次异常的长事务把 claim 卡死。
        // 第一步：挑一条**此刻确实授权**的有效引用，并锁住消息行与频道行。
        //
        // 锁频道行同时覆盖了私聊：私聊的权威成员就写在这一行的
        // `direct_user1/2_id` 上，没有 participants 行（成员判据与投递收件人那份
        // 表达式同形，见 `message_repo` 的 dispatch_recipient 插入）。
        // 排序只为让并发的多个 claim 以相同顺序取锁。
        let candidates: Vec<(i64, i16)> = sqlx::query_as(
            r#"
            SELECT m.channel_id, c.channel_type
            FROM privchat_message_file_refs r
            JOIN privchat_messages m
              ON m.message_id = r.message_id
             AND m.created_at = r.message_created_at
            JOIN privchat_channels c
              ON c.channel_id = m.channel_id
            -- 🔴 跨**同一份内容的所有逻辑记录**找，不是只看传进来那条。
            --
            -- 同一串字节可能已经有好几条记录：alice 发在群 A（file_id=1），
            -- bob 发在群 B（file_id=2）。charlie 只在群 B，他能读的是 2。
            -- 先按 `ORDER BY file_id` 钉死最老那条再判授权，charlie 就会被拒——
            -- 而他明明有权拿到这份内容。物理文件是同一个，授权是按记录算的。
            WHERE r.file_id IN (
                    SELECT file_id FROM privchat_file_uploads WHERE file_hash = $3
                  )
              AND m.deleted = false
              AND m.revoked = false
              AND (
                    (c.channel_type = 0
                     AND $2 IN (c.direct_user1_id, c.direct_user2_id))
                 OR (c.channel_type = 1
                     AND EXISTS (
                           SELECT 1 FROM privchat_group_members g
                           WHERE g.group_id = c.channel_id
                             AND g.user_id = $2
                             AND g.left_at IS NULL
                         ))
                 OR (c.channel_type NOT IN (0, 1)
                     AND EXISTS (
                           SELECT 1 FROM privchat_channel_participants p
                           WHERE p.channel_id = c.channel_id
                             AND p.user_id = $2
                             AND p.left_at IS NULL
                         ))
                  )
            ORDER BY m.message_id
            FOR SHARE OF m, c
            -- 🔴 有上限的遍历（**不是只取一条**），两头都要顾：
            --   只取 1 条 → 第一条在等锁期间失效就直接拒，哪怕还有别的有效引用；
            --   全取     → 一次 claim 锁住热门文件的所有引用消息，挡下大量无关撤回。
            -- 取到上限之外的候选一律不看：那种情况退化成「照常上传」，不是错误。
            LIMIT 16
            "#,
        )
        .bind(source.file_id as i64)
        .bind(uploader_id as i64)
        .bind(&content_hash)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| Self::map_lock_error("锁定取用授权依据失败", e))?;

        // 🔴 只锁**频道行**，绝不再去锁 group_members。
        //
        // 频道行就是成员变更的串行化点，这是 016 那条 migration 明写的设计：
        // 「Advancing the channel row also serializes membership changes」。
        // 退群的顺序是 member(写) → AFTER trigger → channel(写)；claim 如果在
        // 持有 channel 之后再去锁 member，两边就成了 channel→member 与
        // member→channel 的环，PostgreSQL 只能靠中止一方来解，用户看到的是
        // 随机失败的退群或转发。
        //
        // 成员是否有效已经由上面那条查询的 EXISTS 判过，而 `FOR SHARE` 会在拿到
        // 锁后按最新版本重新求值——所以退群一旦先提交，这里就选不出候选。
        //
        // 也因此只取一条：一次 claim 不该把这份文件的所有引用消息全锁住，
        // 热门文件会因此挡下大量无关的撤回。
        let mut authorized = false;
        for (channel_id, channel_type) in &candidates {
            // 私聊：成员就写在刚锁住的频道行上，EvalPlanQual 会按最新版本重求，
            // 不需要再读一次。
            if *channel_type == 0 {
                authorized = true;
                break;
            }
            // 🔴 群聊 / 其它：成员在另一张表上，必须用**一条新语句**重读一次。
            //
            // 上面那条查询的 `FOR SHARE OF m, c` 只让 PostgreSQL 对 m/c 两行做
            // EvalPlanQual；成员判定在 `EXISTS` 子查询里，走的是语句开始时的快照。
            // 等锁期间提交的退群，它看不见——于是「已经退群了还能取用」。
            //
            // 用新语句而不是给成员行加 `FOR SHARE`：READ COMMITTED 下每条语句
            // 取新快照，够看到那次提交；而加锁会变成 channel→member，与退群的
            // member→channel（AFTER trigger 写 membership_version）成环。
            // 串行化由频道行负责——migration 016 就是这么设计的。
            let sql = if *channel_type == 1 {
                "SELECT 1 FROM privchat_group_members
                  WHERE group_id = $1 AND user_id = $2 AND left_at IS NULL"
            } else {
                "SELECT 1 FROM privchat_channel_participants
                  WHERE channel_id = $1 AND user_id = $2 AND left_at IS NULL"
            };
            if sqlx::query_as::<_, (i32,)>(sql)
                .bind(channel_id)
                .bind(uploader_id as i64)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| ServerError::Database(format!("重读成员关系失败: {e}")))?
                .is_some()
            {
                authorized = true;
                break;
            }
        }

        // 一条有效引用都没有：只有「文件从未被任何消息引用过」且取用者就是
        // 上传者时才放行——那是「自己重发自己刚传的东西」，不是给别人的口子。
        if !authorized && candidates.is_empty() {
            {
                let pending_self: Option<(i32,)> = sqlx::query_as(
                    r#"
                    SELECT 1
                    WHERE NOT EXISTS (
                            SELECT 1 FROM privchat_message_file_refs
                             WHERE file_id IN (
                                     SELECT file_id FROM privchat_file_uploads
                                      WHERE file_hash = $4
                                   )
                          )
                      AND $2 = $3
                    "#,
                )
                .bind(source.file_id as i64)
                .bind(uploader_id as i64)
                .bind(source.uploader_id as i64)
                .bind(&content_hash)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| ServerError::Database(format!("复查取用授权失败: {}", e)))?;
                authorized = pending_self.is_some();
            }
        }
        if !authorized {
            // 授权在这期间没了（撤回、删除、退群）。整事务回滚，不留下新的 file_id。
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(
                "服务端没有这份内容，请正常上传".to_string(),
            ));
        }

        // 拿到锁之后复查源**对象**还在不在、还是不是可用状态：等锁期间它可能已经被
        // GC 掉了。查对象行而不是"还有没有别人引用它"——引用为零的对象照样可以被
        // 取用，只要它还在；反过来，对象没了的话有多少引用都不算数。
        let still_there: Option<(String,)> = sqlx::query_as(
            "SELECT status FROM privchat_attachment_objects WHERE object_id = $1",
        )
        .bind(object_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("复查源文件失败: {}", e)))?;
        if still_there.as_ref().map(|(s,)| s.as_str()) != Some("published") {
            tx.rollback().await.ok();
            return Err(ServerError::NotFound(
                "该文件已被删除，请正常上传".to_string(),
            ));
        }

        // 🔴 秒传取用只是**多一条引用**，物理对象一个字节都不动。
        //
        // 以前这里要逐列复制 file_path / 存储源 / 加密版本 / CEK / key id，任何一列漏掉
        // 都会造出一条"指向同一份字节却描述不一致"的记录——曾经漏掉 key id，新记录
        // version=2 却没有密钥 id，下载时给得出 URL 给不出密钥。现在那些列根本不在
        // 这张表上，漏不掉。
        sqlx::query(
            r#"
            INSERT INTO privchat_file_uploads (
                file_id, original_filename, file_type, mime_type,
                object_id, uploader_id, uploaded_at,
                width, height, business_type, claim_key_hash
            ) VALUES ($1, $2, $3, $4, $5, $6, now_millis(), $7, $8, $9, $10)
            ON CONFLICT (uploader_id, claim_key_hash) WHERE claim_key_hash IS NOT NULL
            DO NOTHING
            "#,
        )
        .bind(file_id as i64)
        .bind(&source.original_filename)
        .bind(source.file_type.as_str())
        .bind(&source.mime_type)
        .bind(object_id)
        .bind(uploader_id as i64)
        .bind(source.width.map(|v| v as i32))
        .bind(source.height.map(|v| v as i32))
        .bind(business_type)
        .bind(claim_key_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| ServerError::Database(format!("创建秒传记录失败: {}", e)))?;

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
            .map_err(|e| ServerError::Database(format!("回读秒传记录失败: {}", e)))?;
            row.map(|(id,)| id as u64).unwrap_or(file_id)
        } else {
            file_id
        };

        tx.commit()
            .await
            .map_err(|e| ServerError::Database(format!("提交秒传取用事务失败: {}", e)))?;
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

    /// 按 file_id 查询
    pub async fn get_by_file_id(&self, file_id: u64) -> Result<Option<FileMetadata>> {
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
            dedup_id: Option<String>,
            sealed_sha256: Option<String>,
            sealed_size: i64,
            plaintext_size: Option<i64>,
            file_path: String,
            storage_source_id: i32,
            format_version: Option<i16>,
            encryption_key_id: Option<i16>,
            status: String,
        }
        // 🔴 物理事实一律从对象表读。引用行上已经没有这些列了（migration 032），
        // 所以这条 JOIN 不是优化，是唯一的读法。
        let row = sqlx::query_as::<_, Row>(
            r#"
            SELECT u.file_id, u.original_filename, u.file_type, u.mime_type,
                   u.uploader_id, u.uploader_ip, u.uploaded_at, u.width, u.height,
                   u.business_type, u.business_id,
                   o.object_id, o.dedup_id, o.sealed_sha256, o.sealed_size, o.plaintext_size,
                   o.file_path, o.storage_source_id, o.format_version, o.encryption_key_id,
                   o.status
            FROM privchat_file_uploads u
            JOIN privchat_attachment_objects o ON o.object_id = u.object_id
            WHERE u.file_id = $1
            "#
        )
        .bind(file_id as i64)
        .fetch_optional(self.pool.as_ref())
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
                dedup_id: r.dedup_id,
                sealed_sha256: r.sealed_sha256,
                sealed_size: r.sealed_size as u64,
                plaintext_size: r.plaintext_size.map(|v| v as u64),
                file_path: r.file_path,
                storage_source_id: r.storage_source_id as u32,
                format_version: r.format_version.and_then(|v| u8::try_from(v).ok()),
                encryption_key_id: r.encryption_key_id.and_then(|v| u8::try_from(v).ok()),
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
