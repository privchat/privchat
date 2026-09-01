-- 跨用户秒传的判重键。
--
-- 原来按 file_hash（**密文**摘要）判重，那只在"同一个人重发同一份已加密字节"时命中。
-- 附件加密改成每个对象一个随机 salt 之后，同一份明文由不同人上传会得到不同密文、
-- 不同摘要——按密文判重等于秒传只对自己生效，而秒传最大的收益（省用户上行带宽和
-- 等待时间）恰恰在"别人已经传过这份文件"的场景。
--
-- dedup_id = HMAC-SHA256(dedup_master_key, "privchat-attachment-dedup-v1" || plaintext_sha256)
--
-- 🔴 存的是 HMAC，不是明文摘要本身。直接存明文 SHA-256 的话，这张表泄露之后
-- 任何人都可以拿常见文件做字典匹配，逐条确认"系统里有没有这份内容"。
-- 密钥留在服务端，客户端算不出 dedup_id，也就无法伪造身份。
ALTER TABLE public.privchat_file_uploads
    ADD COLUMN IF NOT EXISTS dedup_id text;

COMMENT ON COLUMN public.privchat_file_uploads.dedup_id IS
    'HMAC(dedup_master_key, "privchat-attachment-dedup-v1" || plaintext_sha256)；跨用户秒传判重键。NULL = 未配置 dedup 密钥或明文对象';

-- 判重查询是"按 dedup_id 找任意一条已存在的行"，只需要能定位，不需要唯一：
-- 同一份内容会有多行（每个引用者一行），它们共享同一个 file_path。
CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_dedup_id
    ON public.privchat_file_uploads (dedup_id)
    WHERE dedup_id IS NOT NULL;
