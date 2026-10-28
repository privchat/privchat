-- Only environment references are stored here, never resolved credentials.
CREATE TABLE public.privchat_component_config (
    component text PRIMARY KEY CHECK (component = 'rtc'),
    revision text NOT NULL,
    settings jsonb NOT NULL
);
