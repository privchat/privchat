-- A retry must return the original pinner/time, not the retrying manager.
-- The no-op conflict update locks and returns that row even for concurrent inserts.
INSERT INTO privchat_group_pinned_messages
    (group_id, message_id, channel_id, pinned_by, pinned_at)
VALUES ($1, $2, $3, $4, $5)
ON CONFLICT (group_id, message_id) DO UPDATE
    SET pinned_by = privchat_group_pinned_messages.pinned_by
RETURNING pinned_by, pinned_at
