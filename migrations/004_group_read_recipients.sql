-- 群已读：发送时收件人判定 + 明细截止时间（READ_STATUS_SPEC §6.5）
--
-- 为什么需要成员区间表：
--   §6.5.3 的人数口径是「**发送时**有权接收这条消息的其他用户」。直接取当前成员表会让
--   退群者从统计里消失、消息之后入群的人混进来。而 privchat_channel_participants 的
--   joined_at/left_at 是**当前**这段关系——重新入群会把 left_at 清空、joined_at 改写，
--   多次进出的历史就没了。
--
-- 为什么用 pts 而不是时间：
--   pts 是频道内的权威事件位置，与消息同一把尺子。用墙钟时间要面对时钟漂移和客户端
--   时间不可信；判定「消息发出时这个人在不在群里」用同一序列最直接。
--
-- 存储量随**成员变更次数**增长，不随「消息数 × 群人数」增长——这正是不做逐消息收件人
-- 快照的原因。
CREATE TABLE IF NOT EXISTS public.privchat_channel_membership_interval (
    id          bigserial PRIMARY KEY,
    channel_id  bigint NOT NULL,
    user_id     bigint NOT NULL,
    -- 加入时频道的 pts 水位；该值**之后**的消息才把这个人算作收件人。
    joined_pts  bigint NOT NULL,
    -- 退出时的 pts；NULL = 仍在群内。
    left_pts    bigint,
    created_at  bigint DEFAULT public.now_millis() NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_membership_interval_channel_user
    ON public.privchat_channel_membership_interval (channel_id, user_id);
CREATE INDEX IF NOT EXISTS idx_membership_interval_channel_open
    ON public.privchat_channel_membership_interval (channel_id) WHERE left_pts IS NULL;

-- 明细截止时间固定在消息上。
--
-- 若按「当前配置」现算，把保留期从 7 天调到 30 天会让早已过期的旧名单重新开放，
-- 违反 §6.5.4。存下来之后，配置变更只影响新消息。
ALTER TABLE public.privchat_messages
    ADD COLUMN IF NOT EXISTS read_detail_expires_at bigint;

-- 存量群没有区间记录。给当前在群的人补一段从 pts=0 开始的开区间，
-- 否则升级后所有历史消息都会算出「零收件人」。
--
-- 注意这只是**近似**：它把当前成员当成"一直都在"，无法还原升级之前的进出历史。
-- 升级之后发生的进出才是精确的。这个偏差写在 READ_STATUS_SPEC §6.5.3 里。
INSERT INTO public.privchat_channel_membership_interval (channel_id, user_id, joined_pts)
SELECT p.channel_id, p.user_id, 0
FROM public.privchat_channel_participants p
WHERE p.left_at IS NULL
  AND NOT EXISTS (
      SELECT 1 FROM public.privchat_channel_membership_interval m
      WHERE m.channel_id = p.channel_id AND m.user_id = p.user_id AND m.left_pts IS NULL
  );
