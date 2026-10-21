CREATE TABLE IF NOT EXISTS public.privchat_calls (
    call_id bigint PRIMARY KEY,
    channel_id bigint NOT NULL REFERENCES public.privchat_channels(channel_id),
    state text NOT NULL CHECK (state IN ('ringing', 'active', 'ended')),
    document jsonb NOT NULL,
    updated_at bigint NOT NULL,
    history_message_id bigint
);
CREATE INDEX IF NOT EXISTS idx_calls_live ON public.privchat_calls (updated_at) WHERE state <> 'ended';
CREATE TABLE IF NOT EXISTS public.privchat_call_commands (
    user_id bigint NOT NULL,
    device_id text NOT NULL,
    command_id text NOT NULL,
    fingerprint text NOT NULL,
    response jsonb NOT NULL,
    created_at bigint NOT NULL,
    PRIMARY KEY (user_id, device_id, command_id)
);
CREATE TABLE IF NOT EXISTS public.privchat_call_effects (
    id bigserial PRIMARY KEY,
    call_id bigint NOT NULL REFERENCES public.privchat_calls(call_id),
    version bigint NOT NULL,
    kind text NOT NULL CHECK (kind IN ('state', 'history', 'delete_room', 'remove_participant')),
    payload jsonb NOT NULL DEFAULT '{}',
    completed boolean NOT NULL DEFAULT false,
    next_attempt_at bigint NOT NULL DEFAULT 0,
    UNIQUE (call_id, version, kind, payload)
);
CREATE INDEX IF NOT EXISTS idx_call_effects_pending ON public.privchat_call_effects(next_attempt_at) WHERE NOT completed;
