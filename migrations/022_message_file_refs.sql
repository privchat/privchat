-- 022: 消息 → 文件的引用表（MEDIA_REFERENCE_AND_FORWARD_SPEC §3.1）
--
-- 现状：文件归属靠 privchat_file_uploads.business_id 这一个 varchar 列，
-- 一个文件只能挂一条消息。转发要让多条消息引用同一个文件，这个模型表达不了。
--
-- 本迁移只建空表。写入（双写）与存量回填是分开的步骤：spec §10 冻结了
-- 「建表 → 双写 → 回填 → 校验 → 切授权」的顺序，双写必须先于回填，
-- 否则回填期间产生的新消息永远补不进来。

CREATE TABLE IF NOT EXISTS privchat_message_file_refs (
    message_id         BIGINT   NOT NULL,
    -- 消息表按 created_at 做 RANGE 分区，主键是 (message_id, created_at)，
    -- 所以外键必须带上这一列。
    message_created_at BIGINT   NOT NULL,
    file_id            BIGINT   NOT NULL,
    -- 0=original 1=thumbnail 2=preview（privchat_protocol::MediaRole）
    role               SMALLINT NOT NULL,
    ordinal            INTEGER  NOT NULL DEFAULT 0,
    created_at         BIGINT   NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (message_id, role, ordinal),
    FOREIGN KEY (message_id, message_created_at)
        REFERENCES privchat_messages (message_id, created_at)
        ON DELETE CASCADE
);

-- 授权走的是「存在一条有效引用」的存在性查询，file_id 是那条查询的入口；
-- 引用计数 GC 也用它。
CREATE INDEX IF NOT EXISTS idx_privchat_message_file_refs_file
    ON privchat_message_file_refs (file_id);
