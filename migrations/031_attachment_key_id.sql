-- 记录每个附件是用哪一把密钥加密的。
--
-- 加密密钥由服务端在签发上传 token 时决定，客户端只用服务端给的那把；
-- 下载时 `file/get_url` 按这一列取出对应密钥返回。把它记在行上，
-- 密钥轮换就不影响存量对象——不必重新加密，也不必把所有历史密钥一起下发。
--
-- NULL = 该行不是 v2（encryption_version 0 明文 / 1 per-file CEK）。
ALTER TABLE public.privchat_file_uploads
    ADD COLUMN IF NOT EXISTS encryption_key_id smallint;

COMMENT ON COLUMN public.privchat_file_uploads.encryption_key_id IS
    'v2 附件加密所用密钥的 id（对应 config [[attachment.keys]].id 与密文 blob 头部）；NULL = 非 v2';

-- version 与 key id 必须一致，错误状态不许落库。
--
-- 没有这条约束时，`version=2 且 key_id 为空` 会被写进去：上传成功、建行成功，
-- 但下载时给得出 URL 给不出密钥，故障点离成因已经很远。
ALTER TABLE public.privchat_file_uploads
    DROP CONSTRAINT IF EXISTS privchat_file_uploads_encryption_key_id_matches_version;
ALTER TABLE public.privchat_file_uploads
    ADD CONSTRAINT privchat_file_uploads_encryption_key_id_matches_version
    CHECK (
        (encryption_version = 2 AND encryption_key_id IS NOT NULL
         AND encryption_key_id BETWEEN 0 AND 255)
     OR (encryption_version <> 2 AND encryption_key_id IS NULL)
    );
