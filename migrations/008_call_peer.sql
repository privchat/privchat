-- Short-lived SDP/ICE journal. No message/outbox copies of sensitive payloads.
CREATE TABLE public.privchat_call_peer_events (
    call_id bigint NOT NULL REFERENCES public.privchat_calls(call_id) ON DELETE CASCADE,
    seq bigint NOT NULL,
    revision bigint NOT NULL,
    kind text NOT NULL CHECK (kind IN ('offer','answer','ice')),
    payload text NOT NULL CHECK (octet_length(payload) <= 65536),
    from_user_id bigint NOT NULL,
    expires_at bigint NOT NULL,
    PRIMARY KEY (call_id, seq)
);
CREATE INDEX ON public.privchat_call_peer_events(expires_at);
CREATE TABLE public.privchat_call_peer_commands (
    call_id bigint NOT NULL REFERENCES public.privchat_calls(call_id) ON DELETE CASCADE,
    user_id bigint NOT NULL,
    device_id text NOT NULL,
    command_id text NOT NULL,
    fingerprint text NOT NULL,
    seq bigint NOT NULL,
    expires_at bigint NOT NULL,
    PRIMARY KEY (user_id, device_id, command_id)
);
CREATE INDEX ON public.privchat_call_peer_commands(expires_at);
CREATE TABLE public.privchat_call_peer_rates (
    call_id bigint NOT NULL REFERENCES public.privchat_calls(call_id) ON DELETE CASCADE,
    user_id bigint NOT NULL,
    operation text NOT NULL,
    bucket bigint NOT NULL,
    count integer NOT NULL,
    PRIMARY KEY (call_id,user_id,operation)
);
