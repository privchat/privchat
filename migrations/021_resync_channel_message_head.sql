-- 重新对齐 message head。
--
-- V019 加列并回填了一次，但当时**没有运行期维护**：代码里对
-- server_latest_message_pts / server_latest_message_id 的引用数是 0。回填值从落地那一刻
-- 起就在变旧——生产环境部署当天就观察到不一致频道数从 8 涨到 17。
--
-- 运行期维护现已补上（message_repo.rs 的 ADVANCE_CHANNEL_MESSAGE_HEAD_SQL，与建消息同
-- 事务），本迁移把 V019 到维护上线之间落下的那段补齐。之后 head 由运行期保持，不需要
-- 再来一次。
--
-- ⚠️ 排序键必须与运行期维护、与 V019 回填三者完全一致：deleted = false 排除软删，
-- revoked 仍参与（撤回只清内容，消息仍占时间线位置），按 (pts, message_id) DESC 取首行。
-- 任何一处漂移，回填值与运行期值就会从此对不上，而这种不一致只在客户端补历史时显形。
--
-- ⚠️ 显式 BEGIN/COMMIT：runner（main.rs）用 `sqlx::raw_sql(sql).execute()`，Rust 侧不包
-- 外围事务。
--
-- ⚠️ IS DISTINCT FROM 守卫不是为了「结果正确」（裸 UPDATE 结果也正确），而是为了
-- **没有副作用**：privchat_channels 上有 BEFORE UPDATE 触发器
-- assign_privchat_channel_entity_sync_version，每个被匹配的行都会重新分配 sync_version。
-- 没有守卫的话，一次 bookkeeping 写失败后的重试会把全部频道的 sync_version 再推一遍，
-- 让所有在线客户端把自己参与的频道全量重新同步——一次自造的惊群。
-- 「重跑结果一样」不等于「重跑无副作用」。

BEGIN;

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

-- 无消息、或消息全被软删的频道不在 heads 里，两列保持原值（通常是 NULL）。
-- 那是合法的「空」状态，不是「未回填」。

COMMIT;
