CREATE TABLE privchat_timeline_recovery_audit (
    request_id uuid PRIMARY KEY,
    event_id bigint NOT NULL,
    user_id bigint NOT NULL,
    channel_id bigint NOT NULL,
    wire_channel_type smallint NOT NULL,
    service_key_fingerprint text NOT NULL,
    created_at bigint NOT NULL,
    notified_sessions integer CHECK (notified_sessions >= 0)
);
