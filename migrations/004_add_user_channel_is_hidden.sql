-- 为 privchat_user_channels 添加 is_hidden 字段
-- 用于持久化频道隐藏状态（替代内存 HashSet）
ALTER TABLE privchat_user_channels ADD COLUMN IF NOT EXISTS is_hidden BOOLEAN DEFAULT false;
