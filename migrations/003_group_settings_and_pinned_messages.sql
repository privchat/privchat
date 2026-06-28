-- P0-4 / P0-5 / P1 群领域扩展
-- 背景：群业务设置此前散落在 ChannelSettings（channel 模型）上，违背 group/channel 边界。
-- 本迁移把群核心设置提升为 privchat_groups 主表的结构化列（高频、参与搜索/加入/邀请/加好友/禁言校验），
-- 并新增"群消息置顶"表。低频开关仍可留在 privchat_groups.settings jsonb。
--
-- 语义对照：
--   免打扰(DND)        = privchat_user_channels.is_muted        （用户通知偏好，保持不动）
--   群全员禁言(all_muted)= privchat_groups.all_muted             （群策略，本次从 ChannelSettings 迁入）
--   单人禁言            = privchat_group_members.mute_until       （已存在，不动）

-- ============================================================
-- P0-4：群核心设置字段（直接进主表，不拆 group_settings 表）
-- ============================================================
ALTER TABLE public.privchat_groups
    ADD COLUMN IF NOT EXISTS allow_search            boolean  NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS join_policy             smallint NOT NULL DEFAULT 1,
    ADD COLUMN IF NOT EXISTS allow_member_invite     boolean  NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS allow_member_add_friend boolean  NOT NULL DEFAULT true,
    ADD COLUMN IF NOT EXISTS all_muted               boolean  NOT NULL DEFAULT false;

COMMENT ON COLUMN public.privchat_groups.allow_search IS '是否允许被搜索到（群发现）';
COMMENT ON COLUMN public.privchat_groups.join_policy IS '加入策略：0=不允许申请加入 1=允许申请需审核 2=允许直接加入';
COMMENT ON COLUMN public.privchat_groups.allow_member_invite IS '是否允许普通成员邀请他人入群';
COMMENT ON COLUMN public.privchat_groups.allow_member_add_friend IS 'false=群成员之间不允许私自加好友（群主/管理员、已有好友、其它来源不受限）';
COMMENT ON COLUMN public.privchat_groups.all_muted IS '群全员禁言（群策略；与 user_channels.is_muted 的免打扰语义不同）';

-- ============================================================
-- P1：群消息置顶（仅群主/管理员可置顶）
-- ============================================================
CREATE TABLE IF NOT EXISTS public.privchat_group_pinned_messages (
    id          bigint  GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    group_id    bigint  NOT NULL,
    message_id  bigint  NOT NULL,
    channel_id  bigint  NOT NULL,
    pinned_by   bigint  NOT NULL,
    pinned_at   bigint  NOT NULL DEFAULT public.now_millis(),
    CONSTRAINT uq_group_pinned_message UNIQUE (group_id, message_id)
);

COMMENT ON TABLE public.privchat_group_pinned_messages IS '群消息置顶表：群主/管理员将消息置顶到群顶部';

CREATE INDEX IF NOT EXISTS idx_group_pinned_messages_group
    ON public.privchat_group_pinned_messages (group_id, pinned_at DESC);
