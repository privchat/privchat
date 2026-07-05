-- RP-12: server-authoritative money message injection.
-- dedup_key makes red-packet/transfer card injection exactly-once: the platform
-- outbox consumer supplies red_packet:{id} / money_transfer:{id}; a repeated
-- inject (consumer retry / multi-node) hits this unique index and returns the
-- existing message instead of creating a second card. NULL for ordinary chat
-- messages (partial index), so normal sends are unaffected.
ALTER TABLE privchat_messages ADD COLUMN IF NOT EXISTS dedup_key TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS uq_privchat_messages_dedup_key
    ON privchat_messages (dedup_key)
    WHERE dedup_key IS NOT NULL;
