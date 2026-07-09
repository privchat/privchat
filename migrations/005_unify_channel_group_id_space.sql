-- 统一 channel/group ID 空间。
--
-- 背景:群创建把 group_id 直接用作 privchat_channels.channel_id(客户端与
-- SDK 均假定 channel_id == group_id),但 privchat_groups 与 privchat_channels
-- 各用独立序列取号 —— 两边迟早撞号。撞上已有 DM 行时,旧实现的
-- ON CONFLICT (channel_id) DO UPDATE 会把 DM 行改写成群(DM 会话被劫持,
-- 该 DM 的历史消息出现在新群里)。
--
-- 修复:group_id 改为从 channels 同一序列取号,channel_id == group_id 语义
-- 保留且全局唯一;序列先推进到两边现存最大值之后。旧 groups 序列保留不删。

SELECT setval(
    'privchat_channels_channel_id_seq',
    GREATEST(
        (SELECT COALESCE(MAX(channel_id), 0) FROM privchat_channels),
        (SELECT COALESCE(MAX(group_id), 0) FROM privchat_groups)
    ) + 1,
    false
);

ALTER TABLE privchat_groups
    ALTER COLUMN group_id SET DEFAULT nextval('privchat_channels_channel_id_seq');
