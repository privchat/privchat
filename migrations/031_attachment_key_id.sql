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
