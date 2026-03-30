-- Copyright 2024 Shanghai Boyu Information Technology Co., Ltd.
-- https://privchat.dev
--
-- Read cursor authority model (read_pts only).

CREATE TABLE IF NOT EXISTS privchat_channel_read_cursor (
    user_id BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    last_read_pts BIGINT NOT NULL,
    last_read_message_id BIGINT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, channel_id)
);

CREATE INDEX IF NOT EXISTS idx_privchat_channel_read_cursor_channel_updated
    ON privchat_channel_read_cursor (channel_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_privchat_channel_read_cursor_user_updated
    ON privchat_channel_read_cursor (user_id, updated_at DESC);

-- Ensure message projection can use channel+pts ordering efficiently.
-- NOTE: privchat_messages is partitioned by created_at, so a UNIQUE constraint
-- on (channel_id, pts) is not valid at parent-table level in PostgreSQL.
CREATE INDEX IF NOT EXISTS idx_privchat_messages_channel_pts
    ON privchat_messages (channel_id, pts);
