-- 016: 群成员事实源一致性 backfill
--
-- 背景：
--   privchat_groups.member_count 与 privchat_group_members 在历史代码里互不
--   维护：建群只写 privchat_groups（member_count=1 默认值），加群 / 退群只更
--   in-memory，导致：
--     - 所有历史 group 都 member_count=1
--     - privchat_group_members 接近空表（只有走 add_member_admin 的少量行）
--
--   admin 列表显示 "1 成员"、详情显示 "暂无成员"，是这两条数据脱节的真实表现。
--
-- 本 migration 做两件事：
--   1) 把每个 group 的 owner 补到 privchat_group_members（如果还没在表里）。
--      role=0 (Owner)，joined_at = group.created_at（保留语义）。
--      ON CONFLICT 跳过：已存在的（不论是否 left_at NULL）都不动。
--   2) 把所有 privchat_groups.member_count 用 group_members 的真实活跃数重算。
--
-- 运行后验证：
--   SELECT g.group_id, g.member_count,
--          (SELECT COUNT(*) FROM privchat_group_members m
--           WHERE m.group_id = g.group_id AND m.left_at IS NULL) AS actual
--   FROM privchat_groups g
--   WHERE g.member_count != (
--       SELECT COUNT(*) FROM privchat_group_members m
--       WHERE m.group_id = g.group_id AND m.left_at IS NULL
--   );
--   --> 应当返 0 行
--
-- 业务流程的根因修复在 channel_service.rs（create / join / leave / admin
-- add / admin remove 全部走事务 + recompute）。本 migration 仅清理历史脏数据。

BEGIN;

-- 1) 给所有现有 group 把 owner 补进 group_members（已存在则跳过）
INSERT INTO privchat_group_members (group_id, user_id, role, joined_at, updated_at)
SELECT
    g.group_id,
    g.owner_id,
    0,                         -- Owner
    COALESCE(g.created_at, now_millis()),
    now_millis()
FROM privchat_groups g
WHERE g.owner_id IS NOT NULL
ON CONFLICT (group_id, user_id) DO NOTHING;

-- 2) 用真实活跃成员数重算 member_count
UPDATE privchat_groups g
SET member_count = sub.cnt,
    updated_at = now_millis()
FROM (
    SELECT
        gm.group_id,
        COUNT(*)::int AS cnt
    FROM privchat_group_members gm
    WHERE gm.left_at IS NULL
    GROUP BY gm.group_id
) sub
WHERE g.group_id = sub.group_id
  AND g.member_count IS DISTINCT FROM sub.cnt;

-- 没有任何活跃成员的 group：member_count 应为 0
UPDATE privchat_groups g
SET member_count = 0,
    updated_at = now_millis()
WHERE NOT EXISTS (
    SELECT 1 FROM privchat_group_members gm
    WHERE gm.group_id = g.group_id AND gm.left_at IS NULL
)
AND COALESCE(g.member_count, 0) != 0;

COMMIT;
