-- 012_group_join_requests.sql
-- 群入群审批申请持久化（#72A，上线前 P0）。
-- 此前 ApprovalService 仅用内存 HashMap 保存待审批申请，server 重启/crash/滚动升级
-- 会丢失全部 pending —— 属不可恢复的业务数据丢失。改为 DB 持久化，ApprovalService
-- 退化为内存缓存（启动从本表加载 pending，运行期 write-through）。
CREATE TABLE IF NOT EXISTS privchat_group_join_requests (
    request_id    TEXT        PRIMARY KEY,          -- UUID
    group_id      BIGINT      NOT NULL,
    user_id       BIGINT      NOT NULL,             -- 申请人
    method_type   TEXT        NOT NULL,             -- 'qrcode' | 'member_invite'
    method_ref    TEXT,                             -- qr_code_id 或 inviter_id
    status        SMALLINT    NOT NULL DEFAULT 0,   -- 0 pending, 1 approved, 2 rejected, 3 expired
    message       TEXT,                             -- 申请留言
    handler_id    BIGINT,                           -- 处理人（approve/reject）
    reject_reason TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at    TIMESTAMPTZ
);

-- 按群 + 状态查待审批（list 主查询）。
CREATE INDEX IF NOT EXISTS idx_pgjr_group_status
    ON privchat_group_join_requests (group_id, status);

-- 按申请人查历史。
CREATE INDEX IF NOT EXISTS idx_pgjr_user
    ON privchat_group_join_requests (user_id);
