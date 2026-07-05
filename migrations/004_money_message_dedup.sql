-- RP-12: server-authoritative money message injection.
-- Exactly-once card injection needs a hard unique key on dedup_key, but
-- privchat_messages is RANGE-partitioned on created_at (a unique index there
-- must include created_at, which would let the same dedup_key repeat across
-- partitions). So the dedup key lives in its own un-partitioned table: the
-- injector inserts (dedup_key, message_id) in the same tx as the message, and
-- ON CONFLICT DO NOTHING makes a repeated inject (consumer retry / multi-node)
-- a no-op that resolves back to the existing message instead of a second card.
-- Ordinary chat messages never touch this table.
CREATE TABLE IF NOT EXISTS privchat_message_dedup (
    dedup_key   TEXT   PRIMARY KEY,
    message_id  BIGINT NOT NULL,
    created_at  BIGINT NOT NULL
);
