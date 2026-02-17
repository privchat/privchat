-- 003: pts key 改为仅 channel_id（channel_type 不参与 key）

BEGIN;

-- 1) privchat_channel_pts: 主键改为 channel_id，移除 channel_type 列
ALTER TABLE privchat_channel_pts
    DROP CONSTRAINT IF EXISTS privchat_channel_pts_pkey;

ALTER TABLE privchat_channel_pts
    DROP COLUMN IF EXISTS channel_type;

ALTER TABLE privchat_channel_pts
    ADD CONSTRAINT privchat_channel_pts_pkey PRIMARY KEY (channel_id);

-- 2) privchat_commit_log: 唯一约束改为 (channel_id, pts)
ALTER TABLE privchat_commit_log
    DROP CONSTRAINT IF EXISTS privchat_commit_log_channel_id_channel_type_pts_key;

ALTER TABLE privchat_commit_log
    ADD CONSTRAINT privchat_commit_log_channel_id_pts_key UNIQUE (channel_id, pts);

DROP INDEX IF EXISTS idx_privchat_commit_log_channel_pts;
CREATE INDEX IF NOT EXISTS idx_privchat_commit_log_channel_pts
    ON privchat_commit_log (channel_id, pts);

COMMIT;

