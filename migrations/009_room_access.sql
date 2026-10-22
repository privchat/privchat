-- Room 持续授权（ROOM_CHANNEL_SPEC §4.8 / MODULE_WORKSPACE_SPEC §10.5）。
-- 必须持票据的 Room：未配置 [room_ticket] 时订阅一律拒绝（fail-closed），既有直播 Room 不受影响。
CREATE TABLE public.privchat_room_policies (
    channel_id bigint PRIMARY KEY,
    ticket_required boolean NOT NULL,
    created_at bigint NOT NULL
);
-- 撤权：签发时间早于 revoked_at 的票据不得再入场；device_id='' 表示该 UID 在此 Room 的全部设备。
CREATE TABLE public.privchat_room_access_revocations (
    channel_id bigint NOT NULL,
    user_id bigint NOT NULL,
    device_id text NOT NULL DEFAULT '',
    revoked_at bigint NOT NULL,
    PRIMARY KEY (channel_id, user_id, device_id)
);
