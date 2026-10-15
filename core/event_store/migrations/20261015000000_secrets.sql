-- Per-app secret environment variables, encrypted with the key in
-- <data_dir>/secrets.key (ChaCha20-Poly1305; the database never holds the key).
-- `sealed` is the 12-byte nonce followed by the ciphertext and tag.
CREATE TABLE IF NOT EXISTS secrets (
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    sealed BLOB NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (project_id, name)
);
