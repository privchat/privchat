-- 004: 删除冗余索引 idx_privchat_messages_sender_time（P0-5 写集瘦身）。
--
-- 该索引与 idx_privchat_messages_sender 完全同定义 (sender_id, created_at DESC)，
-- 是 001 基线里的重复项。messages INSERT 在 channels 行锁持有期间维护全部索引，
-- 每多一个索引就多一份锁内 B-tree 写入与 WAL。删除重复项不改变任何查询的可用
-- 访问路径（planner 在两个同定义索引间任选其一），纯减少写放大。
--
-- 父索引 DROP 会级联删除各分区上 ATTACH 的子索引
-- （privchat_messages_2026_0X_sender_id_created_at_idx1 / default_..._idx1）。

DROP INDEX IF EXISTS public.idx_privchat_messages_sender_time;