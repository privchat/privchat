ALTER TABLE privchat_channel_read_cursor
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

CREATE SEQUENCE IF NOT EXISTS privchat_channel_read_cursor_sync_version_seq;

UPDATE privchat_channel_read_cursor
SET sync_version = nextval('privchat_channel_read_cursor_sync_version_seq')
WHERE sync_version IS NULL;

ALTER TABLE privchat_channel_read_cursor
    ALTER COLUMN sync_version SET NOT NULL;

CREATE OR REPLACE FUNCTION privchat_set_channel_read_cursor_sync_version()
RETURNS trigger AS $$
BEGIN
    NEW.sync_version := nextval('privchat_channel_read_cursor_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_privchat_channel_read_cursor_sync_version
    ON privchat_channel_read_cursor;

CREATE TRIGGER trg_privchat_channel_read_cursor_sync_version
BEFORE INSERT OR UPDATE ON privchat_channel_read_cursor
FOR EACH ROW
EXECUTE FUNCTION privchat_set_channel_read_cursor_sync_version();

CREATE INDEX IF NOT EXISTS idx_privchat_channel_read_cursor_user_sync_version
    ON privchat_channel_read_cursor(user_id, sync_version ASC);

CREATE INDEX IF NOT EXISTS idx_privchat_channel_read_cursor_channel_sync_version
    ON privchat_channel_read_cursor(channel_id, sync_version ASC);
