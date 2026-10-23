-- Per-app SSH deploy keys for cloning private repositories. The private
-- key is sealed like secrets (ChaCha20-Poly1305 with <data_dir>/secrets.key,
-- bound to the project); the public key and fingerprint are not secret.
CREATE TABLE IF NOT EXISTS deploy_keys (
    project_id TEXT PRIMARY KEY NOT NULL,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    sealed BLOB NOT NULL,
    created_at TEXT NOT NULL
);
