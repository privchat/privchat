-- P1-16 根修：统一会话 id 分配器。
--
-- 005 把 channel/group 并入同一 id 空间（群会话 channel_id == group_id），但两表
-- 各自的 serial sequence 仍独立分配：privchat_groups_group_id_seq 落后于
-- privchat_channels_channel_id_seq（DM 在推进后者）。建群 INSERT RETURNING 的
-- group_id 落在 DM 已占段 → 005 守卫报 "channel_id already taken"，且每次失败
-- 只推进 1，要连败数千次才能爬出重叠段——server 重启/新实例后建群直接不可用。
--
-- 修法：privchat_groups.group_id 改为与 channels 共用同一个 sequence；把序列
-- 推进到两表低位 max 之上。room 会话用 snowflake 高位 id（~6e17 段），用 2^40
-- 阈值排除，避免 setval 跳进 snowflake 段。旧的 groups sequence 保留不删（无引用）。
ALTER TABLE privchat_groups
    ALTER COLUMN group_id SET DEFAULT nextval('privchat_channels_channel_id_seq');

SELECT setval(
    'privchat_channels_channel_id_seq',
    GREATEST(
        COALESCE((SELECT MAX(channel_id) FROM privchat_channels WHERE channel_id < 1099511627776), 0),
        COALESCE((SELECT MAX(group_id) FROM privchat_groups WHERE group_id < 1099511627776), 0),
        (SELECT last_value FROM privchat_channels_channel_id_seq)
    ) + 1,
    false
);
