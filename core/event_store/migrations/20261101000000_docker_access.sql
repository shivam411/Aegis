-- Who may see the server's Docker containers and their logs. 0 (the default)
-- means nobody but the owner (the first admin account); 1 is a grant.
ALTER TABLE users ADD COLUMN docker_access INTEGER NOT NULL DEFAULT 0;
