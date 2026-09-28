-- Authentication state for the HTTP API. Unlike domain events this state is
-- mutable (sessions expire, tokens get revoked), so it lives in plain tables.
-- Every credential is stored hashed; the plaintext is shown only once.

CREATE TABLE users (
    username TEXT PRIMARY KEY NOT NULL,
    -- Argon2id PHC string
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL,
    password_changed_at TEXT NOT NULL
);

CREATE TABLE sessions (
    -- SHA-256 (hex) of the session cookie value
    id_hash TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
    csrf_token TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_seen INTEGER NOT NULL
);

CREATE TABLE api_tokens (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    -- SHA-256 (hex) of the token
    token_hash TEXT NOT NULL UNIQUE,
    scope TEXT NOT NULL,
    created_at TEXT NOT NULL,
    last_used_at TEXT,
    revoked_at TEXT
);

CREATE TABLE webhook_secrets (
    project_id TEXT PRIMARY KEY NOT NULL,
    -- Needed in plaintext to verify HMAC signatures.
    secret TEXT NOT NULL,
    created_at TEXT NOT NULL
);
