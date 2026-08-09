-- 029: 区分「内容摘要」与「落盘字节摘要」，并把处理版本移出唯一键
--
-- 两个修正，都是上一版（028）的错。
--
-- 一、**秒传永远命不中**。协议说 sha256 是「压缩后、加密前」的明文摘要，
--     而客户端上传的是随机 nonce 的 AES-GCM 密文，服务端对**收到的字节**求摘要。
--     于是：同一份文件每次密文都不同，且客户端报的明文摘要与服务端登记的密文摘要
--     根本不是一个东西。两个数永远不会相等，秒传命中率恒为 0。
--
--     所以必须分成两个字段，各司其职：
--       content_sha256  客户端声明的**明文最终内容**摘要 —— 秒传按它判身份
--       stored_sha256   服务端对**实际落盘字节**求的摘要 —— 完整性校验用
--
-- 二、`transform_version` **不该进唯一键**（我上一版把它放进去了，是错的）。
--     字节不同，SHA-256 自然就不同；字节相同就该复用。因为「压缩器版本号不同」
--     而把同样的字节存两份，是白占存储。它保留为元数据，不参与身份。

ALTER TABLE privchat_media_blobs
    ADD COLUMN IF NOT EXISTS stored_sha256 VARCHAR(64);

DROP INDEX IF EXISTS uq_privchat_media_blobs_content;

CREATE UNIQUE INDEX IF NOT EXISTS uq_privchat_media_blobs_content
    ON privchat_media_blobs (content_sha256);
