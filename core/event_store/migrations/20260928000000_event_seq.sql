-- Replay must follow insertion order. created_at (RFC-3339 text) is not a
-- reliable ordering key and rowid can be renumbered by VACUUM, so events get
-- an explicit monotonically increasing sequence number.
CREATE TABLE events_v2 (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

INSERT INTO events_v2 (id, event_type, payload_json, created_at)
SELECT id, event_type, payload_json, created_at
FROM events
ORDER BY created_at ASC, rowid ASC;

DROP TABLE events;

ALTER TABLE events_v2 RENAME TO events;
