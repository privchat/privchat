-- Fix historical deployments where file upload schema was not created completely.
-- Some DBs miss table `privchat_file_uploads` and/or sequence `privchat_file_uploads_id_seq`,
-- which breaks upload flow at `nextval(...)`.

CREATE SEQUENCE IF NOT EXISTS privchat_file_uploads_id_seq;

CREATE TABLE IF NOT EXISTS privchat_file_uploads (
    file_id BIGINT PRIMARY KEY DEFAULT nextval('privchat_file_uploads_id_seq'),
    original_filename VARCHAR(512) NOT NULL,
    file_size BIGINT NOT NULL,
    file_type VARCHAR(32) NOT NULL,
    mime_type VARCHAR(128) NOT NULL,
    file_path TEXT NOT NULL,
    storage_source_id INT NOT NULL DEFAULT 0,
    uploader_id BIGINT NOT NULL,
    uploader_ip VARCHAR(45),
    uploaded_at BIGINT NOT NULL DEFAULT now_millis(),
    width INT,
    height INT,
    file_hash VARCHAR(128),
    business_type VARCHAR(64),
    business_id VARCHAR(128)
);

CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_uploader_id
    ON privchat_file_uploads(uploader_id);
CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_uploaded_at
    ON privchat_file_uploads(uploaded_at);
CREATE INDEX IF NOT EXISTS idx_privchat_file_uploads_business
    ON privchat_file_uploads(business_type, business_id);

ALTER TABLE privchat_file_uploads
    ALTER COLUMN file_id SET DEFAULT nextval('privchat_file_uploads_id_seq');

ALTER SEQUENCE privchat_file_uploads_id_seq
    OWNED BY privchat_file_uploads.file_id;

SELECT setval(
    'privchat_file_uploads_id_seq',
    GREATEST(COALESCE((SELECT MAX(file_id) FROM privchat_file_uploads), 0), 0) + 1,
    false
);
