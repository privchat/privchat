-- Friend sync no longer sends a personal assistant's internal username (ACCOUNT_IDENTITY_SPEC 6.5):
-- it is sent as empty. Clients that already hold the generated _pa_... handle only receive the
-- friendship again when its sync_version moves, so move it once for every friendship with an
-- assistant. The trigger assigns the new sync_version on update; no column changes.
UPDATE privchat_friendships
SET status = status
WHERE friend_id IN (SELECT user_id FROM privchat_users WHERE user_type = 3);
