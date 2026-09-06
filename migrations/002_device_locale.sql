-- 设备语言，用于生成推送文案。
--
-- 推送的标题/正文由服务端拼好交给 APNs：iOS 的 alert 是系统直接展示的，App
-- 不参与，所以"用哪种语言"必须在服务端就定下来。没有这一列的话，越南语用户
-- 的锁屏上只会出现中文。
--
-- 存客户端上报的 BCP-47 语言标签（zh-Hans / zh-Hant / en / vi ...）。NULL =
-- 老客户端没报过，按简体中文兜底。
ALTER TABLE privchat_user_devices
    ADD COLUMN IF NOT EXISTS locale character varying(16);
