-- 013: 私聊频道唯一性约束
-- 目标：保证同一对用户只能有一条 channel_type=0 的记录，且方向规范化
--       （direct_user1_id <= direct_user2_id）。
--
-- 注意：步骤 1 会直接 DELETE 每对用户除最老 channel_id 外的重复行。
-- 由于 privchat_channel_participants / privchat_messages /
-- privchat_read_receipts / privchat_user_channels 都是 ON DELETE CASCADE，
-- 重复频道里的参与者、消息、已读回执和用户频道状态会一并被删除。
-- 此迁移仅适用于可接受重复频道数据丢失的环境。

-- 1) 清理重复私聊：每对用户只保留 channel_id 最小的那条
WITH dupes AS (
    SELECT channel_id,
           ROW_NUMBER() OVER (
               PARTITION BY LEAST(direct_user1_id, direct_user2_id),
                            GREATEST(direct_user1_id, direct_user2_id)
               ORDER BY channel_id
           ) AS rn
    FROM privchat_channels
    WHERE channel_type = 0
      AND direct_user1_id IS NOT NULL
      AND direct_user2_id IS NOT NULL
)
DELETE FROM privchat_channels
 WHERE channel_id IN (SELECT channel_id FROM dupes WHERE rn > 1);

-- 2) 规范化方向：让 direct_user1_id <= direct_user2_id 始终成立
UPDATE privchat_channels
   SET direct_user1_id = direct_user2_id,
       direct_user2_id = direct_user1_id
 WHERE channel_type = 0
   AND direct_user1_id IS NOT NULL
   AND direct_user2_id IS NOT NULL
   AND direct_user1_id > direct_user2_id;

-- 3) 唯一部分表达式索引：channel_type=0 时 (min, max) 组合唯一
CREATE UNIQUE INDEX IF NOT EXISTS idx_privchat_channels_direct_unique
    ON privchat_channels (
        LEAST(direct_user1_id, direct_user2_id),
        GREATEST(direct_user1_id, direct_user2_id)
    )
    WHERE channel_type = 0
      AND direct_user1_id IS NOT NULL
      AND direct_user2_id IS NOT NULL;
