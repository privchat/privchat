-- 把投递镜像里的群角色对回真源。
--
-- 群角色有两份存储：`privchat_group_members`（真源，建群/转让/设管理员都写它）
-- 与 `privchat_channel_participants`（投递镜像）。读的一侧——成员列表 RPC 和每一处
-- 「是不是群主/管理员」的鉴权——历史上走的是镜像，于是镜像一旦漂移，群主在所有端
-- 既看不到管理入口、调 API 也被 403，而且没有任何报错指向原因。生产上两个最大的群
-- （807 人 / 622 人）正是这么废掉的。
--
-- 服务端已改为 hydrate 时用真源覆盖镜像，漂移会自愈；这里把落库的那份也对齐，
-- 免得两张表长期不一致继续误导后来的人。幂等：只改真正不同的行。
UPDATE privchat_channel_participants p
   SET role = gm.role
  FROM privchat_channels c
  JOIN privchat_group_members gm ON gm.group_id = c.group_id
 WHERE p.channel_id = c.channel_id
   AND p.user_id = gm.user_id
   AND p.role <> gm.role;
