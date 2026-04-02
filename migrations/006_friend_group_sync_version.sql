CREATE SEQUENCE IF NOT EXISTS privchat_friend_sync_version_seq;
CREATE SEQUENCE IF NOT EXISTS privchat_group_sync_version_seq;

ALTER TABLE privchat_friendships
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

ALTER TABLE privchat_groups
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

UPDATE privchat_friendships
SET sync_version = updated_at
WHERE sync_version IS NULL;

UPDATE privchat_groups
SET sync_version = updated_at
WHERE sync_version IS NULL;

DO $$
DECLARE
    next_friend_sync_version BIGINT;
    next_group_sync_version BIGINT;
BEGIN
    SELECT GREATEST(COALESCE(MAX(sync_version), 0), COALESCE(MAX(updated_at), 0)) + 1
    INTO next_friend_sync_version
    FROM privchat_friendships;

    PERFORM setval(
        'privchat_friend_sync_version_seq',
        GREATEST(next_friend_sync_version, 1),
        false
    );

    SELECT GREATEST(COALESCE(MAX(sync_version), 0), COALESCE(MAX(updated_at), 0)) + 1
    INTO next_group_sync_version
    FROM privchat_groups;

    PERFORM setval(
        'privchat_group_sync_version_seq',
        GREATEST(next_group_sync_version, 1),
        false
    );
END $$;

ALTER TABLE privchat_friendships
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_friend_sync_version_seq');

ALTER TABLE privchat_groups
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_group_sync_version_seq');

CREATE INDEX IF NOT EXISTS idx_privchat_friendships_user_sync_version
    ON privchat_friendships (user_id, sync_version DESC);

CREATE INDEX IF NOT EXISTS idx_privchat_groups_sync_version
    ON privchat_groups (sync_version DESC);

CREATE OR REPLACE FUNCTION assign_privchat_friend_sync_version()
RETURNS TRIGGER AS $$
BEGIN
    NEW.sync_version = nextval('privchat_friend_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION assign_privchat_group_sync_version()
RETURNS TRIGGER AS $$
BEGIN
    NEW.sync_version = nextval('privchat_group_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS privchat_friendships_sync_version_trigger ON privchat_friendships;
CREATE TRIGGER privchat_friendships_sync_version_trigger
    BEFORE UPDATE ON privchat_friendships
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_friend_sync_version();

DROP TRIGGER IF EXISTS privchat_groups_sync_version_trigger ON privchat_groups;
CREATE TRIGGER privchat_groups_sync_version_trigger
    BEFORE UPDATE ON privchat_groups
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_group_sync_version();
