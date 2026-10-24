CREATE TABLE public.privchat_call_push_devices (
    user_id bigint NOT NULL,
    device_id uuid NOT NULL,
    platform text NOT NULL CHECK(platform IN ('apns_voip','fcm')),
    token text NOT NULL,
    session_version bigint NOT NULL,
    updated_at bigint NOT NULL,
    PRIMARY KEY(user_id,device_id),
    FOREIGN KEY(user_id,device_id) REFERENCES public.privchat_devices(user_id,device_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX idx_call_push_token ON public.privchat_call_push_devices(platform,token);
CREATE TABLE public.privchat_call_push_outbox (
    id bigserial PRIMARY KEY,
    call_id bigint NOT NULL REFERENCES public.privchat_calls(call_id),
    user_id bigint NOT NULL,
    device_id uuid NOT NULL,
    kind text NOT NULL CHECK(kind IN ('incoming','cancel')),
    call_version bigint NOT NULL,
    expires_at bigint NOT NULL,
    status text NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','sent','cancelled','expired','failed','unavailable')),
    attempts integer NOT NULL DEFAULT 0,
    next_attempt_at bigint NOT NULL DEFAULT 0,
    last_error text,
    UNIQUE(call_id,user_id,device_id,kind)
);
CREATE INDEX idx_call_push_due ON public.privchat_call_push_outbox(next_attempt_at,id) WHERE status='pending';
COMMENT ON TABLE public.privchat_call_push_outbox IS 'Call-owned device delivery; provider acceptance is not device receipt. No SDP/media credentials in payloads.';
