-- SDK-HISTORY-7 / MESSAGE_HISTORY_AND_SEARCH_ARCHITECTURE_SPEC §15.4.4
--
-- 给 privchat_channels 加「服务端最后一条消息」的锚点，并回填存量频道。
--
-- 为什么需要它：客户端打开会话时要判断「本地窗口是否已覆盖服务端最新消息」。
-- 这个判断**不能**用频道的 commit PTS —— 那是 commit log 游标，reaction / edit /
-- revoke 都会推进它却不产生新消息，拿它判定会让「别人给老消息点个表情」变成
-- 「下次打开重拉一次历史」。message head 与 commit head 必须是两个字段。
--
-- ⚠️ 显式 BEGIN/COMMIT：runner（main.rs）用 `sqlx::raw_sql(sql).execute()`，
-- Rust 侧不包外围事务，加列成功而回填失败会留下半迁移状态。
-- 现有 015-018 都没写显式事务，本文件刻意不跟随。
--
-- ⚠️ 幂等：记录 migration 的 INSERT 在本事务之外，写失败时本文件会被重新执行。
-- 因此 ADD COLUMN 用 IF NOT EXISTS，UPDATE 带 IS DISTINCT FROM 守卫 —— 后者不是
-- 为了「结果正确」（裸 UPDATE 结果也正确），而是为了**没有副作用**：
-- privchat_channels 上有 BEFORE UPDATE 触发器 assign_privchat_channel_entity_sync_version，
-- 每个被匹配的行都会重新分配 sync_version。没有守卫的话，一次 bookkeeping 写失败
-- 的重试就会把全部非空频道的 sync_version 再推一遍，让所有在线客户端把自己参与的
-- 频道全量重新同步一次 —— 一次自造的惊群。
-- 「重跑结果一样」不等于「重跑无副作用」，触发器就是那个副作用。

BEGIN;

ALTER TABLE privchat_channels
  ADD COLUMN IF NOT EXISTS server_latest_message_pts bigint,
  ADD COLUMN IF NOT EXISTS server_latest_message_id  bigint;

COMMENT ON COLUMN privchat_channels.server_latest_message_pts IS
  'message head 的 pts：频道最后一条未删除消息。不是 commit 游标（reaction/edit/revoke 会推进 commit PTS 但不产生新消息）。无消息时为 NULL。';
COMMENT ON COLUMN privchat_channels.server_latest_message_id IS
  'message head 的 server_message_id，与 server_latest_message_pts 成对写入。无消息时为 NULL。';

-- 回填：排序键必须与运行期维护 head 的 canonical 查询完全一致
-- （deleted = false 排除软删；revoked 仍参与——撤回只清内容，消息仍在时间线上占位），
-- 否则回填值与运行期值会从第一天起就不一样。
WITH heads AS (
  SELECT DISTINCT ON (channel_id)
         channel_id, pts, message_id
  FROM privchat_messages
  WHERE deleted = false
  ORDER BY channel_id, pts DESC, message_id DESC
)
UPDATE privchat_channels c
SET server_latest_message_pts = h.pts,
    server_latest_message_id  = h.message_id
FROM heads h
WHERE h.channel_id = c.channel_id
  AND (c.server_latest_message_pts, c.server_latest_message_id)
      IS DISTINCT FROM (h.pts, h.message_id);

-- 无消息、或消息全被软删的频道不在 heads 里，两列保持 NULL。
-- 这是合法状态，不是「未回填」：客户端用独立的 hydrated 布尔区分
-- 「没补过」与「补过、结果是空」。

COMMIT;
