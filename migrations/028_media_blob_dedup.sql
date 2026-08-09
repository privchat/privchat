-- 028: 附件秒传 —— 物理对象与逻辑句柄分层
--
-- 「转发」在本产品里就是**当前用户重新发一条同样的消息**。它不需要转发协议，
-- 需要的是：同一份内容第二次发送时不再上传字节。
--
-- 于是必须把一直挤在一行里的两件事拆开：
--
--   privchat_media_blobs   物理对象：一份字节存一次，按内容摘要认身份
--   privchat_file_uploads  逻辑句柄：谁的文件、叫什么、绑在哪条消息上
--
-- 拆开之前，`privchat_file_uploads` 把 storage_path / uploader_id / file_hash / cek
-- 全放在同一行，两个用户发同一张图必然存两份字节。
--
-- 🔴 `transform_version` 必须参与唯一键。客户端压缩/转码算法一变，产出的字节就变，
-- 那是**另一个**物理对象；不区分的话，新老编码会被当成同一份，取回来的是旧画质。
--
-- 🔴 CEK 挂在 blob 上，不挂句柄。同一份密文只能有一把密钥——放在句柄上，
-- 第二个用户会拿到解不开自己那份密文的 key。
--
-- 存量行的 blob_id 留空：老的 file_hash 写的是 `hash:<u64>`（DefaultHasher，
-- 跨 Rust 版本都不稳定），本来就不能用于内容比对。它们照常读写，只是永远不命中秒传。

CREATE TABLE IF NOT EXISTS privchat_media_blobs (
    blob_id            BIGSERIAL PRIMARY KEY,
    -- 压缩/转码之后、加密之前的最终内容摘要（SHA-256 十六进制，64 字符）。
    content_sha256     VARCHAR(64)  NOT NULL,
    -- 产出这份字节的客户端处理版本；0 = 原始未处理。
    transform_version  INTEGER      NOT NULL DEFAULT 0,
    storage_path       TEXT         NOT NULL,
    storage_source_id  INTEGER      NOT NULL DEFAULT 0,
    file_size          BIGINT       NOT NULL,
    mime_type          VARCHAR(128) NOT NULL,
    encryption_version INTEGER      NOT NULL DEFAULT 0,
    cek                TEXT,
    created_at         BIGINT       NOT NULL DEFAULT now_millis()
);

-- 秒传判定就查这一条索引：同内容 + 同处理版本 = 同一个物理对象。
CREATE UNIQUE INDEX IF NOT EXISTS uq_privchat_media_blobs_content
    ON privchat_media_blobs (content_sha256, transform_version);

ALTER TABLE privchat_file_uploads
    ADD COLUMN IF NOT EXISTS blob_id BIGINT REFERENCES privchat_media_blobs(blob_id);

CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_blob
    ON privchat_file_uploads (blob_id);
