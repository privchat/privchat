-- 为好友申请持久化补充请求消息字段
-- status=0 (Pending) 时该字段存申请文案；Accepted 后可为空

ALTER TABLE privchat_friendships
ADD COLUMN IF NOT EXISTS request_message TEXT;
