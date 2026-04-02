-- 为现有实体同步补单调 version，避免直接拿 updated_at 充当增量游标。

CREATE SEQUENCE IF NOT EXISTS privchat_user_entity_sync_version_seq;
CREATE SEQUENCE IF NOT EXISTS privchat_channel_entity_sync_version_seq;

ALTER TABLE privchat_users
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

ALTER TABLE privchat_channels
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

ALTER TABLE privchat_user_channels
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

UPDATE privchat_users
SET sync_version = updated_at
WHERE sync_version IS NULL;

UPDATE privchat_channels
SET sync_version = updated_at
WHERE sync_version IS NULL;

UPDATE privchat_user_channels
SET sync_version = updated_at
WHERE sync_version IS NULL;

DO $$
DECLARE
    next_user_sync_version BIGINT;
    next_channel_sync_version BIGINT;
BEGIN
    SELECT GREATEST(COALESCE(MAX(sync_version), 0), COALESCE(MAX(updated_at), 0)) + 1
    INTO next_user_sync_version
    FROM privchat_users;

    PERFORM setval(
        'privchat_user_entity_sync_version_seq',
        GREATEST(next_user_sync_version, 1),
        false
    );

    SELECT GREATEST(
        COALESCE((SELECT MAX(sync_version) FROM privchat_channels), 0),
        COALESCE((SELECT MAX(updated_at) FROM privchat_channels), 0),
        COALESCE((SELECT MAX(sync_version) FROM privchat_user_channels), 0),
        COALESCE((SELECT MAX(updated_at) FROM privchat_user_channels), 0)
    ) + 1
    INTO next_channel_sync_version;

    PERFORM setval(
        'privchat_channel_entity_sync_version_seq',
        GREATEST(next_channel_sync_version, 1),
        false
    );
END $$;

ALTER TABLE privchat_users
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_user_entity_sync_version_seq');

ALTER TABLE privchat_channels
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_channel_entity_sync_version_seq');

ALTER TABLE privchat_user_channels
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_channel_entity_sync_version_seq');

CREATE INDEX IF NOT EXISTS idx_privchat_users_sync_version
    ON privchat_users (sync_version);

CREATE INDEX IF NOT EXISTS idx_privchat_channels_sync_version
    ON privchat_channels (sync_version);

CREATE INDEX IF NOT EXISTS idx_privchat_user_channels_user_sync_version
    ON privchat_user_channels (user_id, sync_version DESC);

CREATE OR REPLACE FUNCTION assign_privchat_user_sync_version()
RETURNS TRIGGER AS $$
BEGIN
    NEW.sync_version = nextval('privchat_user_entity_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION assign_privchat_channel_entity_sync_version()
RETURNS TRIGGER AS $$
BEGIN
    NEW.sync_version = nextval('privchat_channel_entity_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS privchat_users_sync_version_trigger ON privchat_users;
CREATE TRIGGER privchat_users_sync_version_trigger
    BEFORE UPDATE ON privchat_users
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_user_sync_version();

DROP TRIGGER IF EXISTS privchat_channels_sync_version_trigger ON privchat_channels;
CREATE TRIGGER privchat_channels_sync_version_trigger
    BEFORE UPDATE ON privchat_channels
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_channel_entity_sync_version();

DROP TRIGGER IF EXISTS privchat_user_channels_sync_version_trigger ON privchat_user_channels;
CREATE TRIGGER privchat_user_channels_sync_version_trigger
    BEFORE UPDATE ON privchat_user_channels
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_channel_entity_sync_version();
