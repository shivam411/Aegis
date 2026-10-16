-- Users get a role: viewer (read-only), deployer (deploy, roll back, start
-- and stop apps) or admin (everything, including users, settings and
-- secrets). Accounts that existed before roles keep full access.
ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'viewer';
UPDATE users SET role = 'admin';
