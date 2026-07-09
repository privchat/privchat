-- Guard device session_state against invalid enum values.
-- NOT VALID avoids scanning existing rows during rollout, while still enforcing
-- the constraint for new and updated rows.
ALTER TABLE public.privchat_devices
    ADD CONSTRAINT privchat_devices_session_state_check
    CHECK (session_state IN (0, 1, 2, 3, 4)) NOT VALID;
