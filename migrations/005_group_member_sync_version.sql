CREATE SEQUENCE IF NOT EXISTS privchat_group_member_sync_version_seq;

ALTER TABLE privchat_group_members
    ADD COLUMN IF NOT EXISTS updated_at BIGINT NOT NULL DEFAULT now_millis();

ALTER TABLE privchat_group_members
    ADD COLUMN IF NOT EXISTS sync_version BIGINT;

UPDATE privchat_group_members
SET
    updated_at = COALESCE(left_at, joined_at, now_millis()),
    sync_version = COALESCE(left_at, joined_at, now_millis())
WHERE sync_version IS NULL;

DO $$
DECLARE
    next_group_member_sync_version BIGINT;
BEGIN
    SELECT GREATEST(
        COALESCE(MAX(sync_version), 0),
        COALESCE(MAX(updated_at), 0),
        COALESCE(MAX(joined_at), 0),
        COALESCE(MAX(left_at), 0)
    ) + 1
    INTO next_group_member_sync_version
    FROM privchat_group_members;

    PERFORM setval(
        'privchat_group_member_sync_version_seq',
        GREATEST(next_group_member_sync_version, 1),
        false
    );
END $$;

ALTER TABLE privchat_group_members
    ALTER COLUMN sync_version SET NOT NULL,
    ALTER COLUMN sync_version SET DEFAULT nextval('privchat_group_member_sync_version_seq');

CREATE INDEX IF NOT EXISTS idx_privchat_group_members_group_sync_version
    ON privchat_group_members (group_id, sync_version DESC);

CREATE OR REPLACE FUNCTION assign_privchat_group_member_sync_version()
RETURNS TRIGGER AS $$
BEGIN
    NEW.sync_version = nextval('privchat_group_member_sync_version_seq');
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS update_privchat_group_members_updated_at ON privchat_group_members;
CREATE TRIGGER update_privchat_group_members_updated_at
    BEFORE UPDATE ON privchat_group_members
    FOR EACH ROW EXECUTE FUNCTION update_updated_at_column();

DROP TRIGGER IF EXISTS privchat_group_members_sync_version_trigger ON privchat_group_members;
CREATE TRIGGER privchat_group_members_sync_version_trigger
    BEFORE UPDATE ON privchat_group_members
    FOR EACH ROW
    EXECUTE FUNCTION assign_privchat_group_member_sync_version();
