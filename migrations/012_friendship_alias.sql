-- 012: 好友备注字段
-- 为好友关系表添加 alias 列，用于存储用户对好友设置的备注名

ALTER TABLE privchat_friendships ADD COLUMN IF NOT EXISTS alias VARCHAR(64);
