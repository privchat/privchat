-- CODEX-9 P4: durable protocol-level Delivered receipts.

CREATE TABLE IF NOT EXISTS privchat_message_delivery_receipts (
    -- privchat_messages is range-partitioned by created_at and therefore has
    -- no database-unique message_id-only key that PostgreSQL can reference.
    -- The INSERT path validates the message row transactionally instead.
    server_message_id BIGINT NOT NULL,
    receipt_type TEXT NOT NULL,
    channel_id BIGINT NOT NULL,
    sender_id BIGINT NOT NULL,
    recipient_user_id BIGINT NOT NULL,
    ack_session_id BIGINT NOT NULL,
    delivered_at BIGINT NOT NULL,
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    PRIMARY KEY (server_message_id, receipt_type)
);

CREATE INDEX IF NOT EXISTS idx_privchat_delivery_receipts_sender
    ON privchat_message_delivery_receipts (sender_id, delivered_at DESC);

COMMENT ON TABLE privchat_message_delivery_receipts IS
    'Protocol-level receiver ACK receipts; first ACK wins and transport write success is insufficient';
