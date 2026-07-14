-- 013_group_join_request_unique_pending.sql
-- #72A.1 群审批一致性加固（GA 前）。
--
-- 缺口：同一 (group_id, user_id) 可堆积多条 PENDING 申请（无约束）；approve/reject 仅靠内存
-- write 锁串行，多实例部署下失效 → 两个管理员并发 approve 可能双写、双加群。
--
-- 本迁移在 DB 层加「同群同人至多一条待审批」偏唯一约束，作为并发的最终裁决者；配合
-- approve/reject 的 DB CAS（UPDATE ... WHERE status=0）与 approve+入群同事务，实现多实例安全。
--
-- 注意：历史数据若已存在同 (group_id,user_id) 的多条 pending，需先收敛再建唯一索引（本地/
-- 测试库无历史；生产走 migrate，若冲突会报错，此时先跑下方收敛 UPDATE 再重跑迁移）。

-- 收敛历史重复 pending：同 (group_id,user_id) 保留最新一条 pending，其余较早的置为 expired(3)。
UPDATE privchat_group_join_requests t
SET status = 3, updated_at = now()
WHERE status = 0
  AND EXISTS (
    SELECT 1 FROM privchat_group_join_requests t2
    WHERE t2.group_id = t.group_id
      AND t2.user_id = t.user_id
      AND t2.status = 0
      AND (t2.created_at > t.created_at
           OR (t2.created_at = t.created_at AND t2.request_id > t.request_id))
  );

-- 同群同人至多一条 PENDING(status=0)。已处理（approved/rejected/expired）不受限，允许再次申请。
CREATE UNIQUE INDEX IF NOT EXISTS uq_pgjr_active_pending
    ON privchat_group_join_requests (group_id, user_id)
    WHERE status = 0;
