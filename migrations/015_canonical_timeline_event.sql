-- CODEX-9 P1: additive canonical timeline event beside legacy JSON fields.
ALTER TABLE privchat_commit_log
    ADD COLUMN IF NOT EXISTS event_schema_version SMALLINT,
    ADD COLUMN IF NOT EXISTS canonical_event BYTEA;

COMMENT ON COLUMN privchat_commit_log.event_schema_version IS
    'FlatBuffers CanonicalTimelineEvent schema version; NULL for legacy-only commits';
COMMENT ON COLUMN privchat_commit_log.canonical_event IS
    'FlatBuffers CanonicalTimelineEvent bytes; legacy message_type/content remain during rollout';
