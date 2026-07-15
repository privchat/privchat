-- CODEX-9 P2: commit-time recipient snapshot and transactional dispatch outbox.

ALTER TABLE privchat_channels
    ADD COLUMN IF NOT EXISTS membership_version BIGINT NOT NULL DEFAULT 0;

-- Group membership is authoritative in privchat_group_members. Advancing the
-- channel row also serializes membership changes against message snapshotting,
-- which locks the same channel row FOR UPDATE.
CREATE OR REPLACE FUNCTION privchat_bump_group_membership_version()
RETURNS TRIGGER AS $$
DECLARE
    target_group_id BIGINT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        target_group_id := OLD.group_id;
    ELSE
        target_group_id := NEW.group_id;
    END IF;
    UPDATE privchat_channels
    SET membership_version = membership_version + 1
    WHERE channel_id = target_group_id AND channel_type = 1;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_privchat_group_membership_version ON privchat_group_members;
CREATE TRIGGER trg_privchat_group_membership_version
AFTER INSERT OR DELETE OR UPDATE OF left_at ON privchat_group_members
FOR EACH ROW EXECUTE FUNCTION privchat_bump_group_membership_version();

CREATE TABLE IF NOT EXISTS privchat_message_dispatch_outbox (
    event_id BIGINT PRIMARY KEY
        REFERENCES privchat_commit_log(id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL,
    channel_type SMALLINT NOT NULL,
    pts BIGINT NOT NULL,
    sender_id BIGINT NOT NULL,
    event_kind SMALLINT NOT NULL CHECK (event_kind IN (1, 2, 3)),
    membership_version BIGINT NOT NULL,
    status SMALLINT NOT NULL DEFAULT 0 CHECK (status IN (0, 1, 2)),
    created_at BIGINT NOT NULL DEFAULT now_millis(),
    dispatched_at BIGINT
);

CREATE TABLE IF NOT EXISTS privchat_message_dispatch_recipient (
    event_id BIGINT NOT NULL
        REFERENCES privchat_message_dispatch_outbox(event_id) ON DELETE CASCADE,
    user_id BIGINT NOT NULL,
    state SMALLINT NOT NULL DEFAULT 0 CHECK (state IN (0, 1, 2, 3)),
    lease_owner TEXT,
    lease_until BIGINT,
    lease_token BIGINT NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at BIGINT NOT NULL DEFAULT 0,
    last_error TEXT,
    PRIMARY KEY (event_id, user_id)
);

CREATE INDEX IF NOT EXISTS idx_privchat_dispatch_recipient_claim
    ON privchat_message_dispatch_recipient (next_attempt_at, event_id, user_id)
    WHERE state = 0;

CREATE INDEX IF NOT EXISTS idx_privchat_dispatch_outbox_retention
    ON privchat_message_dispatch_outbox (status, created_at);

COMMENT ON TABLE privchat_message_dispatch_outbox IS
    'Committed timeline event dispatch aggregate; DISPATCHED is transport/offline handoff, not protocol Delivered';
COMMENT ON TABLE privchat_message_dispatch_recipient IS
    'Per-recipient dispatch state, retry schedule and fenced lease truth';
