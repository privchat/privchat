-- 设备级推送提示音开关。
--
-- 「声音」这个开关以前只作用于 App 内的本地提示音，而 APNs payload 里固定带
-- `"sound": "default"`：用户在设置里关掉声音，锁屏推送照样响。开关看起来没生效，
-- 而且这件事只能在服务端修——iOS 的通知由系统展示，App 无法拦截自己的远程通知。
--
-- 设备级而不是账号级：一台设备静音不该影响另一台。NULL/缺省 = 开（保持现有行为）。
ALTER TABLE privchat_user_devices
    ADD COLUMN IF NOT EXISTS push_sound boolean NOT NULL DEFAULT true;
