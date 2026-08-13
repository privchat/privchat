-- 031: 整包 / 分片上传完成的幂等键
--
-- 取消一次性 token 消费之后，旧整包路径**失去了它唯一的防重放**：24 小时内把同一个
-- 请求体 POST 两次，会建出两条文件行。分片路径的 complete 同样需要「响应丢了、
-- 客户端重试要拿回同一个 file_id」。
--
-- 🔴 **绝不复用 `claim_key_hash`。** 形状虽然一样，但那一列的语义是**秒传命中**：
-- `claim_key_hash IS NOT NULL` 是 MEDIA_REFERENCE_AND_FORWARD_SPEC §0.2 与发布验收
-- 唯一可信的判据。让完成路径也往里写，整传的文件会伪装成秒传命中，把这条判据毁掉。
--
-- 键取 `SHA256(upload_id)`：`upload_id` 本身就是这次上传的唯一标识
--（签名 token 直接签入；旧 UUID token 由服务端稳定派生）。

ALTER TABLE privchat_file_uploads
    ADD COLUMN IF NOT EXISTS upload_completion_key VARCHAR(64);

CREATE UNIQUE INDEX IF NOT EXISTS uq_privchat_file_uploads_completion_key
    ON privchat_file_uploads (uploader_id, upload_completion_key)
    WHERE upload_completion_key IS NOT NULL;
