-- Re-align channel entity sync sequence with existing stored sync_version/updated_at values.
-- Older code paths explicitly inserted privchat_user_channels.sync_version without advancing
-- the shared sequence, which made later UPDATE-triggered nextval() smaller than existing
-- channel versions and caused channel pin/mute changes to disappear from entity sync.

SELECT setval(
    'privchat_channel_entity_sync_version_seq',
    GREATEST(
        COALESCE((SELECT MAX(sync_version) FROM privchat_channels), 0),
        COALESCE((SELECT MAX(updated_at) FROM privchat_channels), 0),
        COALESCE((SELECT MAX(sync_version) FROM privchat_user_channels), 0),
        COALESCE((SELECT MAX(updated_at) FROM privchat_user_channels), 0)
    ) + 1,
    false
);
