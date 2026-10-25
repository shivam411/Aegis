-- Which apps a user may see, and password-hidden apps.
--
-- `users.all_apps` = 1 (the default, and what every existing account keeps)
-- means the user sees every app. With 0 they see only the apps listed in
-- `user_apps`. Admins always see everything.
ALTER TABLE users ADD COLUMN all_apps INTEGER NOT NULL DEFAULT 1;

CREATE TABLE user_apps (
    username TEXT NOT NULL,
    project_id TEXT NOT NULL,
    PRIMARY KEY (username, project_id)
);

-- An app with a row here is hidden: it is left out of every list and every
-- request about it fails as "not found" until the session unlocks it with
-- the password.
CREATE TABLE app_locks (
    project_id TEXT PRIMARY KEY NOT NULL,
    -- Argon2id PHC string
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);

-- Apps a session has unlocked; ends with the session.
CREATE TABLE session_unlocks (
    id_hash TEXT NOT NULL,
    project_id TEXT NOT NULL,
    PRIMARY KEY (id_hash, project_id)
);
