-- ATTACHMENT_ENCRYPTION v1 Minimal: per-file CEK on file uploads.
-- encryption_version: 0 = legacy plaintext; 1 = AES-256-GCM (client-side).
-- cek: base64url(no-pad) of the 32-byte content key; nonce travels in the blob header.
-- See spec/03-protocol-sdk/ATTACHMENT_ENCRYPTION_SPEC.md
ALTER TABLE public.privchat_file_uploads ADD COLUMN encryption_version integer DEFAULT 0 NOT NULL;
ALTER TABLE public.privchat_file_uploads ADD COLUMN cek text;
