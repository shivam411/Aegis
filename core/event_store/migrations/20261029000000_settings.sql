-- Small switches an admin changes at run time (for example whether the
-- Prometheus /metrics endpoint is on).
CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
